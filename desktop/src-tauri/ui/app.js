const rawInvoke = window.__TAURI__.core.invoke;
async function invoke(method, args = {}) {
  const read = ['snapshot', 'chat', 'github_repositories', 'self_chat_status', 'llm_providers', 'audit'].includes(method);
  let result = await rawInvoke('command', { request: {
    method, args, revision: read ? null : current?.revision ?? null, contract: 1
  }});
  if (result.code === 'authorization_pending' && ['connect_microsoft', 'finish_github_login'].includes(method)) {
    const finish = method === 'connect_microsoft' ? 'finish_microsoft' : 'finish_github_login';
    const deadline = Date.now() + 600_000;
    message('Autorización pendiente en el navegador.');
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
let current;
const session = crypto.randomUUID();

function message(text) {
  const notice = $('#notice');
  notice.textContent = text;
  notice.hidden = false;
}

function field(id, value) { $(id).value = value ?? ''; }
function lines(text) { return text.split('\n').map(s => s.trim()).filter(Boolean); }
function names(text) { return text.split(',').map(s => s.trim()).filter(Boolean); }

function showTab(name) {
  if (name === 'messages') loadMessages().catch(message);
  for (const section of ['settings', 'messages', 'knowledge', 'chat']) {
    $('#' + section).hidden = section !== name;
  }
  for (const button of document.querySelectorAll('nav button')) {
    button.setAttribute('aria-current', button.dataset.tab === name ? 'page' : 'false');
  }
}

function rowText(primary, secondary) {
  const div = document.createElement('div');
  const strong = document.createElement('strong');
  strong.textContent = primary;
  const small = document.createElement('small');
  small.textContent = secondary;
  div.append(strong, small);
  return div;
}

function renderCredentials() {
  const container = $('#credentials');
  container.replaceChildren();
  for (const [name, source] of Object.entries(current.credentials)) {
    const row = document.createElement('div');
    row.className = 'row';
    const origins = { system: 'Configurada en el almacén del sistema', environment: 'Configurada mediante variable de entorno', file: 'Configurada mediante archivo privado', missing: 'Sin configurar', unused: 'Sin configurar · la cadena de modelos actual no la usa' };
    const empty = source === 'missing' || source === 'unused';
    row.append(rowText(name, origins[source] || 'Configurada'));
    const input = document.createElement('input');
    input.type = 'password'; input.autocomplete = 'new-password';
    input.placeholder = empty ? 'Introduce la credencial' : 'Guardada; escribe aquí solo para reemplazarla';
    input.setAttribute('aria-label', `Nuevo valor de ${name}`);
    const save = document.createElement('button');
    save.textContent = empty ? 'Guardar' : 'Reemplazar';
    save.disabled = true;
    input.oninput = () => { save.disabled = !input.value.trim(); };
    save.onclick = async () => {
      try {
        await invoke('set_credential', { name, value: input.value });
        input.value = '';
        await reload();
        message(`${name} guardada en el sistema.`);
      } catch (error) { message(error); }
    };
    const remove = document.createElement('button');
    remove.className = 'secondary'; remove.textContent = 'Quitar';
    remove.onclick = async () => {
      try { await invoke('delete_credential', { name }); await reload(); }
      catch (error) { message(error); }
    };
    row.append(input, save, remove);
    container.append(row);
  }
}

const providerNames = { codex: 'Codex', claude: 'Claude Code', deepseek: 'DeepSeek' };
const effortNames = { none: 'Sin razonamiento', minimal: 'Mínimo', low: 'Bajo', medium: 'Medio', high: 'Alto', xhigh: 'Muy alto', max: 'Máximo' };
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
  if (!info) return 'Proveedor no disponible en esta versión';
  if (info.transport === 'cli') return info.available ? `CLI instalada${info.version ? ' · ' + info.version : ''}` : 'CLI no instalada en este equipo';
  const source = current.credentials[info.credential];
  return source && source !== 'missing' && source !== 'unused' ? `API · ${info.credential} configurada` : `API · falta ${info.credential} en Credenciales`;
}

function select(caption, label, options, value) {
  const field = document.createElement('label'); field.className = 'field';
  const small = document.createElement('small'); small.textContent = caption;
  const element = document.createElement('select');
  element.setAttribute('aria-label', label);
  for (const [optionValue, text] of options) element.append(new Option(text, optionValue));
  element.value = value;
  field.append(small, element);
  return [field, element];
}

function renderChain() {
  const list = $('#llm-chain');
  list.replaceChildren();
  let active = 0;
  llmRows.forEach((row, index) => {
    const info = llmCatalog.providers.find(provider => provider.id === row.provider);
    const name = providerNames[row.provider] || row.provider;
    const item = document.createElement('li');
    item.className = 'chain-row' + (row.enabled ? '' : ' off');
    const rank = document.createElement('span'); rank.className = 'rank';
    rank.textContent = !row.enabled ? 'Inactivo' : active++ === 0 ? 'Predeterminado' : `Respaldo ${active - 1}`;
    const label = document.createElement('label'); label.className = 'check';
    const toggle = document.createElement('input'); toggle.type = 'checkbox'; toggle.checked = row.enabled;
    toggle.disabled = !row.enabled && !(info?.available);
    toggle.setAttribute('aria-label', `Activar ${name}`);
    toggle.onchange = () => { row.enabled = toggle.checked; renderChain(); };
    const title = document.createElement('div');
    const strong = document.createElement('strong'); strong.textContent = name;
    const small = document.createElement('small'); small.textContent = providerStatus(info);
    title.append(strong, small);
    label.append(toggle, title);
    const models = (info?.models ?? []).map(model => [model.id, model.name]);
    if (row.model && !models.some(([id]) => id === row.model)) models.unshift([row.model, `${row.model} (actual)`]);
    const [modelField, model] = select('Modelo', `Modelo de ${name}`, models, row.model);
    model.title = row.model;
    const efforts = () => info?.models.find(m => m.id === row.model)?.efforts ?? info?.efforts ?? [row.effort];
    const effortOptions = () => {
      const values = efforts();
      return (values.includes(row.effort) ? values : [row.effort, ...values]).map(value => [value, effortNames[value] || value]);
    };
    const [effortField, effort] = select('Esfuerzo', `Esfuerzo de ${name}`, effortOptions(), row.effort);
    model.onchange = () => {
      row.model = model.value;
      const supported = efforts();
      if (!supported.includes(row.effort)) row.effort = info?.models.find(m => m.id === row.model)?.default_effort ?? supported[0];
      renderChain();
    };
    effort.onchange = () => { row.effort = effort.value; };
    const order = document.createElement('div'); order.className = 'order';
    for (const [text, offset, aria] of [['↑', -1, 'Subir prioridad'], ['↓', 1, 'Bajar prioridad']]) {
      const button = document.createElement('button'); button.className = 'secondary'; button.textContent = text;
      button.setAttribute('aria-label', `${aria} de ${name}`);
      button.disabled = !llmRows[index + offset];
      button.onclick = () => {
        [llmRows[index], llmRows[index + offset]] = [llmRows[index + offset], llmRows[index]];
        renderChain();
      };
      order.append(button);
    }
    item.append(rank, label, modelField, effortField, order);
    list.append(item);
  });
}

async function loadChain() {
  // Without detection the configured order still loads, so saving never drops it.
  try { llmCatalog = await invoke('llm_providers'); }
  catch (error) { message(error); }
  llmRows = chainRows(current.config);
  renderChain();
}

const statusLabels = {
  sent: ['Respondido', 'ok'], dry_run: ['En observación (no enviado)', 'info'], sending: ['Enviando', 'active'],
  processing: ['En curso', 'active'], pending: ['En cola', 'active'], failed: ['Error', 'error'],
  uncertain: ['Envío incierto', 'error'],
};
const reasonLabels = {
  ineligible_message: 'No dirigido al asistente: propio, de un grupo sin mención, antiguo, vacío o una respuesta de la app.',
  sensitive_question: 'La pregunta contiene datos sensibles; queda para ti.',
  no_authorized_resource: 'No hay fuentes autorizadas para esta conversación.',
  informational_message: 'Mensaje informativo: no pide respuesta.',
  deterministic_greeting: 'Saludo respondido con el texto configurado.',
  supported_answer: 'Respuesta generada.',
  unsafe_proposal: 'Retenida: contenía datos sensibles o no cabía en un mensaje de Teams.',
  unsafe_answer: 'Retenida: contenía datos sensibles o no cabía en un mensaje de Teams.',
  reference_selection_failed: 'Bloqueada por Jev al elegir referencias (versión anterior).',
  final_gate: 'Bloqueada por el control final de Jev (versión anterior).',
  message_changed: 'El mensaje cambió, se borró o dejó de ser elegible antes de enviar.',
  send_result_unknown_manual_review: 'No se sabe si el envío llegó: revísalo en Teams (no se reintenta).',
  read_or_provider_error: 'Error al leer Teams o al llamar a un proveedor; se reintenta.',
  generating: 'Redactando la respuesta.', retrieving: 'Buscando en las fuentes.', tool_started: 'Consultando una herramienta.',
  selecting_references: 'Eligiendo referencias.', final_review: 'Revisión final.',
  holding_reply: 'Enviando aviso de espera.',
};
const kindLabels = { self: 'Chat personal', direct: 'Directo', group: 'Grupo', unsupported: 'Otro' };
let messageRows = [];
const openMessages = new Set();

function messageState(row) {
  const reason = row.audit.reason || '';
  if (row.status === 'ignored') {
    if (reason === 'ineligible_message') return ['No dirigido', 'muted'];
    if (/^(unsafe_|invalid_references|reference_selection_failed|final_gate)/.test(reason)) return ['Retenido', 'warn'];
    return ['Sin respuesta', 'muted'];
  }
  return statusLabels[row.status] || [row.status, 'muted'];
}

function reasonText(reason) {
  if (!reason) return '';
  if (reason.startsWith('invalid_references')) return `Retenida: referencias o enlaces no verificables (${reason.replace('invalid_references: ', '')}).`;
  return reasonLabels[reason] || reason;
}

function when(row) {
  const millis = row.audit.received_at ?? row.created_at * 1000;
  return new Date(millis).toLocaleString();
}

function messageField(container, label, text, pre = false) {
  if (!text) return;
  const block = document.createElement('div'); block.className = 'field-block';
  const title = document.createElement('strong'); title.textContent = label;
  const body = document.createElement(pre ? 'pre' : 'p'); body.textContent = text;
  block.append(title, body); container.append(block);
}

function renderMessages() {
  const list = $('#messages-list');
  const filter = $('#messages-filter').value;
  const hideIneligible = $('#messages-hide-ineligible').checked;
  const rows = messageRows.filter(row => {
    const [, tone] = messageState(row);
    if (hideIneligible && row.audit.reason === 'ineligible_message' && row.status === 'ignored') return false;
    return filter === 'all' || (filter === 'sent' && tone === 'ok') || (filter === 'withheld' && (tone === 'warn' || (tone === 'muted' && row.audit.reason !== 'ineligible_message')))
      || (filter === 'error' && tone === 'error') || (filter === 'active' && tone === 'active');
  });
  const counts = messageRows.reduce((acc, row) => { const [label] = messageState(row); acc[label] = (acc[label] || 0) + 1; return acc; }, {});
  $('#messages-summary').textContent = `${messageRows.length} mensajes recientes · ` + Object.entries(counts).map(([label, n]) => `${label}: ${n}`).join(' · ');
  list.replaceChildren();
  if (!rows.length) { list.append(document.createTextNode('No hay mensajes para este filtro.')); return; }
  for (const row of rows) {
    const audit = row.audit;
    const [label, tone] = messageState(row);
    const item = document.createElement('details'); item.className = 'message';
    item.open = openMessages.has(row.resource);
    item.ontoggle = () => { if (item.open) openMessages.add(row.resource); else openMessages.delete(row.resource); };
    const summary = document.createElement('summary');
    const badge = document.createElement('span'); badge.className = `badge ${tone}`; badge.textContent = label;
    const kind = document.createElement('span'); kind.className = 'kind';
    kind.textContent = kindLabels[audit.conversation_kind] || (row.resource.includes('48:notes') ? 'Chat personal' : row.resource.startsWith('simulation:') ? 'Simulación' : 'Teams');
    const time = document.createElement('time'); time.textContent = when(row);
    const text = document.createElement('span'); text.className = 'excerpt';
    const reply = audit.sent || audit.proposed;
    text.textContent = audit.question
      || (reply ? `Respuesta: ${reply.replace(/\s+/g, ' ').slice(0, 140)}` : null)
      || (audit.reason === 'ineligible_message' ? '(no se guarda el texto de mensajes no dirigidos al asistente)' : '(sin texto registrado)');
    summary.append(badge, kind, time, text);
    item.append(summary);
    const body = document.createElement('div'); body.className = 'message-body';
    messageField(body, 'Mensaje', audit.question, true);
    if (audit.resolved_question) messageField(body, 'Interpretado con el contexto', `${audit.resolved_question}${audit.topic ? ` · tema buscado: ${audit.topic}` : ''}`);
    messageField(body, 'Resultado', reasonText(audit.reason));
    if (row.status === 'sent') messageField(body, 'Respuesta enviada', audit.sent || audit.proposed, true);
    else messageField(body, row.status === 'dry_run' ? 'Propuesta (modo observación)' : 'Respuesta propuesta (no enviada)', audit.proposed, true);
    const details = [];
    if (audit.history_messages) details.push(`Contexto: ${audit.history_messages} mensajes previos`);
    if (audit.provider) details.push(`Modelo: ${audit.provider}`);
    if (audit.provider_fallbacks?.length) details.push(`Fallaron antes: ${audit.provider_fallbacks.join(', ')}`);
    if (audit.source) details.push(`Fuentes: ${audit.source}`);
    if (audit.references?.length) details.push(`Referencias: ${audit.used_sources?.length ?? 0} citadas de ${audit.references.length}${audit.reference_selection === 'code' ? ' (elegidas por el código; Jev no disponible)' : ''}`);
    if (audit.final_check) details.push(`Revisión Jev (informativa): ${audit.final_check === 'unavailable' ? 'no disponible' : audit.final_check}`);
    const notice = { sending: 'enviando', sent: 'enviado', uncertain: 'incierto (no se reintenta)' };
    if (audit.holding_reply) details.push(`Aviso de espera: ${notice[audit.holding_reply] || audit.holding_reply}`);
    if (audit.withheld_notice) details.push(`Aviso de respuesta retenida: ${notice[audit.withheld_notice] || audit.withheld_notice}`);
    messageField(body, 'Detalle', details.join('\n'), true);
    if (audit.trace?.length) {
      const block = document.createElement('div'); block.className = 'field-block';
      const title = document.createElement('strong'); title.textContent = 'Registro';
      const log = document.createElement('ol'); log.className = 'trace';
      for (const step of audit.trace) {
        const entry = document.createElement('li');
        const at = document.createElement('time'); at.textContent = new Date(step.at).toLocaleTimeString();
        const name = document.createElement('b'); name.textContent = step.step;
        entry.append(at, name, document.createTextNode(step.detail ? ` — ${step.detail}` : ''));
        log.append(entry);
      }
      block.append(title, log); body.append(block);
    } else {
      messageField(body, 'Registro', 'Este mensaje se procesó con una versión anterior, sin registro de pasos.');
    }
    const resource = document.createElement('small'); resource.className = 'resource-id'; resource.textContent = row.resource;
    body.append(resource);
    item.append(body);
    list.append(item);
  }
}

async function loadMessages() {
  messageRows = await invoke('audit', { limit: 100, resource: null, content: true });
  renderMessages();
}

function renderRepositories() {
  const container = $('#repositories');
  container.replaceChildren();
  const select = $('#source-repo');
  select.replaceChildren();
  for (const [alias, path] of Object.entries(current.map.repositories)) {
    const option = new Option(alias, alias);
    select.append(option);
    const row = document.createElement('div');
    row.className = 'row';
    row.append(rowText(alias, 'Checkout Git'));
    const input = document.createElement('input');
    input.value = path; input.setAttribute('aria-label', `Ruta de ${alias}`);
    input.onchange = () => { current.map.repositories[alias] = input.value.trim(); };
    const remove = document.createElement('button');
    remove.className = 'secondary'; remove.textContent = 'Quitar';
    remove.onclick = () => {
      delete current.map.repositories[alias];
      current.map.resources = current.map.resources.filter(r => r.repository !== alias);
      renderRepositories(); renderResources();
    };
    const update = document.createElement('button'); update.className = 'secondary'; update.textContent = 'Actualizar';
    update.onclick = async () => {
      update.disabled = true;
      try { await invoke('update_github_repository', { alias }); message(`${alias} actualizado.`); }
      catch (error) { message(error); }
      finally { update.disabled = false; }
    };
    row.append(input, update, remove);
    container.append(row);
  }
}

function renderResources() {
  const container = $('#resources');
  const choices = $('#chat-sources');
  container.replaceChildren(); choices.replaceChildren();
  for (const resource of current.map.resources) {
    if (resource.kind !== 'file') continue;
    const block = document.createElement('div');
    block.className = 'resource';
    const title = document.createElement('strong'); title.textContent = resource.id;
    const detail = document.createElement('small'); detail.textContent = `${resource.repository} / ${resource.path}`;
    block.append(title, detail);
    const description = document.createElement('input');
    description.value = resource.description; description.setAttribute('aria-label', `Descripción de ${resource.id}`);
    description.onchange = () => { resource.description = description.value; };
    const topics = document.createElement('input');
    topics.value = resource.topics.join(', '); topics.setAttribute('aria-label', `Temas de ${resource.id}`);
    topics.onchange = () => { resource.topics = names(topics.value); };
    const audience = document.createElement('input');
    audience.value = resource.allowed_conversations.join(', ');
    audience.placeholder = 'Chats autorizados para Teams, separados por coma';
    audience.setAttribute('aria-label', `Conversaciones autorizadas para ${resource.id}`);
    audience.onchange = () => { resource.allowed_conversations = names(audience.value); };
    const actions = document.createElement('div'); actions.className = 'actions';
    for (const [key, text] of [['enabled', 'Habilitada'], ['external_processing', 'Autorizar procesamiento externo']]) {
      const label = document.createElement('label'); label.className = 'check';
      const box = document.createElement('input'); box.type = 'checkbox'; box.checked = resource[key];
      box.onchange = () => { resource[key] = box.checked; renderResources(); };
      label.append(box, document.createTextNode(text)); actions.append(label);
    }
    const remove = document.createElement('button'); remove.className = 'secondary'; remove.textContent = 'Quitar fuente';
    remove.onclick = () => { current.map.resources = current.map.resources.filter(r => r !== resource); renderResources(); };
    actions.append(remove);
    block.append(description, topics, audience, actions);
    container.append(block);
    if (resource.enabled && resource.external_processing) {
      const label = document.createElement('label'); label.className = 'check';
      const box = document.createElement('input'); box.type = 'checkbox'; box.value = resource.id;
      label.append(box, document.createTextNode(`${resource.id}: ${resource.description}`));
      choices.append(label);
    }
  }
}

function renderStatus(snapshot) {
  const expected = snapshot.config.graph.discover_all_chats ? 1 : snapshot.config.graph.allowed_chats.length + snapshot.config.graph.channels.length;
  let status = snapshot.running ? 'Asistente activo' : 'Asistente detenido';
  if (snapshot.running && expected) {
    if (snapshot.active_subscriptions >= expected) {
      status += ` · Teams: ${snapshot.active_subscriptions} suscripción(es) vigentes`;
    } else {
      const code = snapshot.subscription_issue?.match(/graph_http_(\d{3})/)?.[1];
      status += code ? ` · Teams pendiente (Graph HTTP ${code})` : ' · Teams pendiente de suscripción';
    }
  }
  $('#status').textContent = status;
  $('#start').disabled = snapshot.running;
  $('#stop').disabled = !snapshot.running;
}

async function reload() {
  current = await invoke('snapshot');
  renderStatus(current);
  $('#github-list').disabled = !current.github_connected;
  $('#github-disconnect').disabled = !current.github_connected;
  const config = current.config;
  field('#jev-model', config.jev.model);
  field('#max-answer-chars', config.policy.max_answer_chars);
  field('#max-detailed-answer-chars', config.policy.max_detailed_answer_chars);
  field('#tenant-id', config.graph.tenant_id); field('#client-id', config.graph.client_id);
  field('#user-id', config.graph.user_id); field('#public-url', config.server.public_url);
  field('#bind', config.server.bind); field('#llm-style', config.llm.style);
  field('#tunnel-config', current.tunnel_config);
  $('#cloudflare-tunnel').checked = config.server.cloudflare_tunnel;
  field('#allowed-chats', config.graph.allowed_chats.join('\n'));
  $('#dry-run').checked = config.policy.dry_run;
  const personal = await invoke('self_chat_status');
  const diagnostic = personal.diagnostics;
  $('#self-chat-state').textContent = config.graph.self_chat
    ? `Habilitado: ${config.graph.self_chat.id} · Recepción: webhook y consulta cada 10 s · Última consulta: ${diagnostic?.cursor ? new Date(diagnostic.cursor[1] * 1000).toLocaleString() : 'pendiente'} · Salidas sin resolver: ${diagnostic?.unresolved_outputs ?? 0}`
    : 'Deshabilitado';
  $('#discover-chats').checked = config.graph.discover_all_chats;
  renderCredentials(); renderRepositories(); renderResources();
  await loadChain();
}
setInterval(() => {
  if (current) invoke('snapshot').then(renderStatus).catch(() => {});
}, 15_000);

function collectSettings() {
  const config = current.config;
  config.policy.max_answer_chars = Number($('#max-answer-chars').value);
  config.policy.max_detailed_answer_chars = Number($('#max-detailed-answer-chars').value);
  config.jev.model = $('#jev-model').value.trim();
  config.llm.chain = llmRows.map(({ provider, model, effort, enabled }) => ({ provider, model, effort, enabled }));
  config.llm.style = $('#llm-style').value.trim();
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
  if (!llmRows.some(row => row.enabled)) throw 'Activa al menos un modelo de lenguaje.';
  collectSettings();
  await invoke('save_settings', { config: current.config, map: current.map, tunnelConfig: $('#tunnel-config').value.trim() });
  await reload();
  message('Configuración guardada.');
}

function addTurn(kind, label, text) {
  const row = document.createElement('div'); row.className = 'turn ' + kind;
  row.textContent = `${label}: ${text}`;
  $('#history').append(row);
}

document.querySelectorAll('nav button').forEach(button => button.onclick = () => showTab(button.dataset.tab));
$('#save-settings').onclick = () => save().catch(message);
$('#save-knowledge').onclick = () => save().catch(message);
$('#save-llm').onclick = () => save().catch(message);
$('#messages-refresh').onclick = () => loadMessages().catch(message);
$('#messages-filter').onchange = renderMessages;
$('#messages-hide-ineligible').onchange = renderMessages;
setInterval(() => {
  if (!$('#messages').hidden && $('#messages-auto').checked) loadMessages().catch(() => {});
}, 10_000);
$('#detect-llm').onclick = async () => {
  try { llmCatalog = await invoke('llm_providers'); renderChain(); message('CLI detectadas de nuevo.'); }
  catch (error) { message(error); }
};
$('#start').onclick = async () => {
  try { await save(); await invoke('start_assistant'); await reload(); }
  catch (error) { message(error); }
};
$('#stop').onclick = async () => {
  try { await invoke('stop_assistant'); await reload(); }
  catch (error) { message(error); }
};
$('#teams-login').onclick = async () => {
  try { await save(); await invoke('connect_microsoft'); await reload(); message('Cuenta Microsoft conectada.'); }
  catch (error) { message(error); }
};
$('#import').onclick = async () => {
  try { await invoke('import_existing', { path: $('#legacy-path').value.trim() }); await reload(); message('Configuración importada.'); }
  catch (error) { message(error); }
};
$('#add-repo').onclick = () => {
  const alias = $('#repo-alias').value.trim();
  const path = $('#repo-path').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(alias) || !path) return message('Indica un alias válido y la ruta del checkout.');
  if (current.map.repositories[alias]) return message('Ese alias ya existe.');
  current.map.repositories[alias] = path;
  field('#repo-alias', ''); field('#repo-path', ''); renderRepositories();
};
$('#github-connect').onclick = async () => {
  try {
    const clientId = $('#github-client-id').value.trim();
    const prompt = await invoke('begin_github_login', { clientId });
    $('#github-code').textContent = prompt.user_code;
    $('#github-device').hidden = false;
    message('Código listo. Autoriza la GitHub App con los repositorios deseados.');
  } catch (error) { message(error); }
};
$('#github-open').onclick = () => invoke('open_github_login').catch(message);
$('#github-finish').onclick = async () => {
  const button = $('#github-finish'); button.disabled = true;
  try {
    await invoke('finish_github_login');
    $('#github-device').hidden = true;
    await reload();
    await listGithub();
    message('GitHub conectado.');
  } catch (error) { message(error); }
  finally { button.disabled = false; }
};
async function listGithub() {
  const repos = await invoke('github_repositories');
  const container = $('#github-repositories');
  container.replaceChildren();
  if (!repos.length) { container.append(document.createTextNode('No hay repositorios instalados para esta GitHub App.')); return; }
  for (const repo of repos) {
    const row = document.createElement('div'); row.className = 'row';
    row.append(rowText(repo.full_name, repo.private ? 'Privado' : 'Público'));
    const alias = document.createElement('input');
    alias.value = repo.full_name.split('/').pop().toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^[^a-z]+/, 'repo-').slice(0, 64);
    alias.setAttribute('aria-label', `Alias local de ${repo.full_name}`);
    const clone = document.createElement('button'); clone.className = 'secondary'; clone.textContent = 'Clonar y agregar';
    clone.onclick = async () => {
      clone.disabled = true;
      try { await invoke('clone_github_repository', { fullName: repo.full_name, alias: alias.value.trim() }); await reload(); message(`${repo.full_name} agregado.`); }
      catch (error) { message(error); }
      finally { clone.disabled = false; }
    };
    row.append(alias, clone); container.append(row);
  }
}
$('#github-list').onclick = () => listGithub().catch(message);
$('#github-disconnect').onclick = async () => {
  try { await invoke('disconnect_github'); $('#github-repositories').replaceChildren(); await reload(); message('GitHub desconectado. Los checkouts locales permanecen.'); }
  catch (error) { message(error); }
};
$('#add-source').onclick = () => {
  const id = $('#source-id').value.trim();
  const repository = $('#source-repo').value;
  const path = $('#source-path').value.trim();
  const description = $('#source-description').value.trim();
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id) || !repository || !path || !description) return message('Completa ID, repositorio, archivo y descripción.');
  if (current.map.resources.some(r => r.id === id)) return message('Ese ID ya existe.');
  current.map.resources.push({ id, description, topics: names($('#source-topics').value), enabled: false,
    external_processing: false, allowed_conversations: [], allowed_senders: [], kind: 'file', repository, path });
  for (const name of ['#source-id', '#source-path', '#source-topics', '#source-description']) field(name, '');
  renderResources();
};
$('#chat-form').onsubmit = async event => {
  event.preventDefault();
  const text = $('#message').value.trim();
  const sources = [...$('#chat-sources').querySelectorAll('input:checked')].map(x => x.value);
  if (!text || !sources.length) return message('Escribe un mensaje y selecciona al menos una fuente habilitada.');
  const send = $('#send'); send.disabled = true;
  addTurn('you', 'Tú', text); field('#message', '');
  try {
    const result = await invoke('chat', { input: { session, text, group: false, mentioned: false, sources } });
    addTurn(result.answer ? 'answer' : '', 'Asistente', result.answer || `Sin respuesta (${result.reason}).`);
  } catch (error) { message(error); }
  finally { send.disabled = false; }
};
reload().catch(message);

$('#self-chat-enable').onclick = async () => {
  try { await save(); await invoke('self_chat_enable', { id: $('#self-chat-id').value.trim() || null }); await reload(); message('Chat personal validado y habilitado. Las audiencias de fuentes se conservan.'); }
  catch (error) { message(error); }
};
$('#self-chat-disable').onclick = async () => {
  try { await invoke('self_chat_disable'); await reload(); }
  catch (error) { message(error); }
};
$('#self-chat-test').onclick = async () => {
  try { const result = await invoke('test_self_chat'); message(result.account_scope_verified ? 'Chat personal validado. Escribe una pregunta nueva en Teams para comprobar recepción y respuesta.' : 'Prueba incompleta.'); }
  catch (error) { message(error); }
};
