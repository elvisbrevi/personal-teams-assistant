const rawInvoke = window.__TAURI__.core.invoke;
async function invoke(method, args = {}) {
  const read = ['snapshot', 'chat', 'github_repositories', 'self_chat_status'].includes(method);
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
  for (const section of ['settings', 'knowledge', 'chat']) {
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
    const origins = { system: 'Configurada en el almacén del sistema', environment: 'Configurada mediante variable de entorno', file: 'Configurada mediante archivo privado' };
    row.append(rowText(name, source === 'missing' ? 'Sin configurar' : origins[source] || 'Configurada'));
    const input = document.createElement('input');
    input.type = 'password'; input.autocomplete = 'new-password';
    input.placeholder = source === 'missing' ? 'Introduce la credencial' : 'Guardada; escribe aquí solo para reemplazarla';
    input.setAttribute('aria-label', `Nuevo valor de ${name}`);
    const save = document.createElement('button');
    save.textContent = source === 'missing' ? 'Guardar' : 'Reemplazar';
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
  field('#jev-model', config.jev.model); field('#llm-model', config.llm.model);
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
}
setInterval(() => {
  if (current) invoke('snapshot').then(renderStatus).catch(() => {});
}, 15_000);

function collectSettings() {
  const config = current.config;
  config.policy.max_answer_chars = Number($('#max-answer-chars').value);
  config.policy.max_detailed_answer_chars = Number($('#max-detailed-answer-chars').value);
  config.jev.model = $('#jev-model').value.trim();
  config.llm.model = $('#llm-model').value.trim();
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
