// Settings › Variables: stored variables, their consumers and deliveries.
import { t, tn, tx } from './i18n.js';
import { invoke } from './control.js';
import { $, badge, el, icon, rowText } from './dom.js';
import { current, tab } from './state.js';
import { busy, done } from './notices.js';
import { copy } from './markdown.js';
import { reload, save } from './profile.js';
import { accountEpoch, adoptAccount } from './settings-account.js';
import { loadMigration } from './settings-migration.js';
import { ago } from './messages.js';
import { toolNames } from './knowledge.js';

// ---------------------------------------------------------------- account variables

/** Metadata only: lists never carry a value, and the forms clear it as soon as it is stored. */
let variableState = { variables: [], pending: [], error: '', sync: null };
/** One entry ID per attempt, so repeating an interrupted save cannot create a duplicate. */
let variableAttempt = null;
/**
 * The one value on screen after an explicit Reveal. It lives only in this variable and the DOM
 * (never in storage, a URL or history) and goes away on Hide, navigation, a hidden page, logout
 * or after VALUE_SECONDS.
 */
let revealedVariable = null;
let revealEpoch = 0;
const VALUE_SECONDS = 30;
/** Entry being edited, armed for deletion, and the description typed so far (kept across a conflict). */
let editingVariable = null;
let deletingVariable = null;
let variableDraft = null;
let armTimer = null;
let clipboardCleanup = null;
const VARIABLE_NAME = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;
const UNSAFE_TEXT = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/;

function clearRevealedVariable(render = true) {
  revealEpoch += 1;
  if (!revealedVariable) return;
  clearTimeout(revealedVariable.timer);
  revealedVariable = null;
  if (render) renderVariables();
}
/** Pagehide and logout also drop what was typed but not yet saved. */
function clearTypedVariableValues() {
  clearRevealedVariable(false);
  for (const field of document.querySelectorAll('#panel-variables textarea')) field.value = '';
  editingVariable = deletingVariable = variableDraft = null;
  renderVariables();
}
document.addEventListener('visibilitychange', () => { if (document.hidden) clearRevealedVariable(); });
window.addEventListener('pagehide', clearTypedVariableValues);

async function digest(text) {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.from(new Uint8Array(bytes), byte => byte.toString(16).padStart(2, '0')).join('');
}
/**
 * Best effort: a minute after Copy value, clear the clipboard only if it still holds exactly what
 * was copied. Only a fingerprint is kept, and unrelated clipboard contents are never touched.
 */
async function scheduleClipboardCleanup(value) {
  clearTimeout(clipboardCleanup);
  if (!crypto.subtle || !navigator.clipboard?.readText) return;
  const fingerprint = await digest(value);
  clipboardCleanup = setTimeout(async () => {
    try { if (await digest(await navigator.clipboard.readText()) === fingerprint) await navigator.clipboard.writeText(''); } catch { /* left alone without permission or focus */ }
  }, 60_000);
}

/** A change names the version the user saw; another client's newer change refreshes the list instead of being overwritten. */
async function changeVariable(method, variable, extra = {}) {
  try {
    return await invoke(method, { id: variable.id, version: variable.version, ...extra });
  } catch (error) {
    await loadVariables();
    throw error;
  }
}

async function loadVariables() {
  const epoch = accountEpoch;
  try {
    const reply = await invoke('variable_list');
    if (!adoptAccount(epoch, reply.standing)) return;
    variableState = { variables: reply.variables, pending: reply.pending, error: reply.migration_error ?? '', sync: reply.sync ?? null };
  } catch (error) {
    if (epoch !== accountEpoch) return;
    variableState = { variables: [], pending: [], error: String(error?.message ?? error), sync: null };
  }
  renderVariables();
  await loadConsumers();
  await loadMigration();
}

// ---------------------------------------------------------------- variable consumers

/** Which variable each consumer uses (name and version only) and the approved local launchers. */
let consumerState = { consumers: [], launchers: [], launchersHere: false, error: '' };
let deliveryState = { destinations: [], approveHere: false };
/** The destination and variable whose delivery waits for a second tap. */
let armedDelivery = null;
/** The variable picked in each selector, kept across refreshes until it is applied. */
const consumerChoice = {};
/** The last result shown under each consumer: a status sentence, never a value. */
const consumerNote = {};

async function loadConsumers() {
  const epoch = accountEpoch;
  try {
    const reply = await invoke('variable_consumer_list');
    if (!adoptAccount(epoch, reply.standing)) return;
    consumerState = { consumers: reply.consumers, launchers: reply.launchers ?? [], launchersHere: reply.launchers_available === true, error: '' };
  } catch (error) {
    if (epoch !== accountEpoch) return;
    consumerState = { consumers: [], launchers: [], launchersHere: false, error: String(error?.message ?? error) };
  }
  try {
    // Destinations belong to the installation's owner; another account simply has none.
    const reply = await invoke('variable_delivery_list');
    if (epoch !== accountEpoch) return;
    deliveryState = { destinations: reply.destinations, approveHere: reply.approve_here };
  } catch {
    if (epoch !== accountEpoch) return;
    deliveryState = { destinations: [], approveHere: false };
  }
  renderConsumers();
}

const CHECK_RESULTS = {
  ok: '{destination} accepted the key.',
  rejected: '{destination} rejected the key.',
  failed: '{destination} answered with an unexpected error.',
  unreachable: 'Could not reach {destination}.',
};

function consumerRow(consumer) {
  const { id, binding } = consumer;
  const title = el('span', { class: 'cred-name', textContent: tx(consumer.title), translate: 'no' });
  const usable = binding?.state === 'ok';
  const configured = variableState.variables.find(v => v.name.toUpperCase() === consumer.credential);
  const status = !binding ? (configured ? badge(t('Using {name} · version {n}', { name: configured.name, n: configured.version }), 'ok') : badge(t('No variable chosen'), 'muted'))
    : usable ? badge(t('Using {name} · version {n}', { name: binding.variable.name, n: binding.variable.version }), 'ok')
    : badge(binding.state === 'deleted' ? t('Variable deleted') : t('Variable missing'), 'warn');
  const chosen = consumerChoice[id] ?? binding?.id ?? '';
  const select = el('select', { 'aria-label': t('Variable for {consumer}', { consumer: consumer.title }) },
    el('option', { value: '', textContent: t('Choose a variable') }),
    variableState.variables.map(v => el('option', { value: v.id, textContent: `${v.name} · v${v.version}`, selected: v.id === chosen, translate: 'no' })));
  const use = el('button', { type: 'button', textContent: t('Use this variable') });
  const sync = () => { use.disabled = !select.value || (usable && select.value === binding.id); };
  select.onchange = () => { consumerChoice[id] = select.value; sync(); };
  sync();
  use.onclick = () => busy(use, async () => {
    const variable = variableState.variables.find(v => v.id === select.value);
    const reply = await invoke('variable_consumer_bind', { consumer: id, variable: select.value, version: variable?.version });
    delete consumerChoice[id];
    consumerNote[id] = t('{consumer} now uses {name}.', { consumer: consumer.title, name: reply.binding.variable.name }) + (reply.restart_required ? ' ' + t('Restart the assistant to apply this change.') : '');
    await loadConsumers();
  });
  const actions = [select, use];
  if (binding) {
    const test = el('button', { type: 'button', class: 'secondary', textContent: t('Test key'), disabled: !usable,
      'aria-label': t('Test key with {destination}', { destination: consumer.destination }) });
    test.onclick = () => busy(test, async () => {
      const reply = await invoke('variable_consumer_check', { consumer: id });
      consumerNote[id] = t(CHECK_RESULTS[reply.result.status] ?? CHECK_RESULTS.failed, { destination: consumer.destination });
      await loadConsumers();
    });
    const stop = el('button', { type: 'button', class: 'secondary', textContent: t('Stop using') });
    stop.onclick = () => busy(stop, async () => {
      const reply = await invoke('variable_consumer_unbind', { consumer: id });
      delete consumerChoice[id];
      consumerNote[id] = t('{consumer} now uses its configured variable.', { consumer: consumer.title }) + (reply.restart_required ? ' ' + t('Restart the assistant to apply this change.') : '');
      await loadConsumers();
    });
    actions.push(test, stop);
  }
  return el('div', { class: 'row variable-row' },
    rowText(title, tx(consumer.summary), status),
    el('p', { class: 'hint variable-note', textContent: t('Sent only to {destination}. The value is never shown here.', { destination: consumer.destination }) }),
    el('div', { class: 'variable-actions' }, actions),
    consumerNote[id] ? el('p', { class: 'hint variable-note', role: 'status', textContent: consumerNote[id] }) : null);
}

/** `here`: the launcher can also be removed from this client (the machine itself, not the portal). */
function launcherRow(launcher, here) {
  const title = el('span', { class: 'cred-name', textContent: launcher.id, translate: 'no' });
  const uses = launcher.variables.map(v => v.state === 'ok'
    ? t('{env} from {name}', { env: v.env, name: v.variable.name })
    : t('{env}: variable deleted or missing', { env: v.env })).join(', ');
  const remove = el('button', { type: 'button', class: 'danger', textContent: t('Remove'), 'aria-label': t('Remove launcher {name}', { name: launcher.id }) });
  remove.onclick = () => busy(remove, async () => {
    await invoke('variable_launcher_remove', { id: launcher.id });
    await loadConsumers();
    done(t('Launcher {name} removed.', { name: launcher.id }));
  });
  const status = launcher.approved ? badge(t('Approved'), 'ok') : badge(t('Waiting for your approval'), 'warn');
  const actions = here ? [remove] : [];
  if (!launcher.approved) {
    // Only a person at this window or the owner's portal approves; the digest names the definition shown here.
    const approve = el('button', { type: 'button', textContent: t('Approve'), 'aria-label': t('Approve launcher {name}', { name: launcher.id }) });
    approve.onclick = () => busy(approve, async () => {
      await invoke('variable_launcher_approve', { id: launcher.id, digest: launcher.digest });
      await loadConsumers();
      done(t('Launcher {name} approved.', { name: launcher.id }));
    });
    actions.unshift(approve);
  }
  const command = [launcher.program, ...(launcher.args ?? [])].join(' ');
  return el('div', { class: 'row variable-row' },
    rowText(title, launcher.description, status),
    el('p', { class: 'hint variable-note', textContent: command, translate: 'no' }),
    el('p', { class: 'hint variable-note', textContent: launcher.directory, translate: 'no' }),
    el('p', { class: 'hint variable-note', textContent: uses, translate: 'no' }),
    el('div', { class: 'variable-actions' }, actions));
}

const DELIVERY_RESULTS = {
  ok: 'Delivered to {destination}.',
  rejected: '{destination} refused the delivery credential.',
  failed: '{destination} answered with an error.',
  unreachable: 'Could not reach {destination}. Check it before delivering again.',
};

/** A variable reference as the host reports it: its name, or that it is gone. */
function deliveryVariable(reference) {
  return reference?.state === 'ok' ? reference.name : t('variable deleted or missing');
}

/** Everything the approval covers, as plain text: a person approves exactly what is shown. */
function deliveryLines(destination) {
  const target = destination.target;
  const lines = [`${target.method} ${destination.url}`];
  if (target.kind === 'http') {
    for (const [name, value] of Object.entries(target.headers ?? {})) lines.push(`${name}: ${value}`);
    if (target.auth) lines.push(`${target.auth.header}: ${target.auth.prefix}‹${deliveryVariable(target.auth.variable)}›`);
    lines.push(t('Body: {body}', { body: JSON.stringify(target.body) }));
  } else {
    lines.push(t('Cloudflare Worker {script}, secret {secret}', { script: target.script, secret: target.secret_name }));
    lines.push(`Authorization: Bearer ‹${deliveryVariable(target.token)}›`);
  }
  lines.push(t('May receive: {names}', { names: destination.variables.map(deliveryVariable).join(', ') }));
  return lines;
}

function deliveryRow(destination) {
  const { id } = destination;
  const title = el('span', { class: 'cred-name', textContent: id, translate: 'no' });
  const status = destination.approved ? badge(t('Approved'), 'ok') : badge(t('Waiting for your approval'), 'warn');
  const actions = [];
  if (!destination.approved && deliveryState.approveHere) {
    const approve = el('button', { type: 'button', textContent: t('Approve'), 'aria-label': t('Approve destination {name}', { name: id }) });
    approve.onclick = () => busy(approve, async () => {
      await invoke('variable_delivery_approve', { id, digest: destination.digest });
      await loadConsumers();
      done(t('Destination {name} approved.', { name: id }));
    });
    actions.push(approve);
  }
  if (destination.approved) {
    for (const variable of destination.variables.filter(v => v.state === 'ok')) {
      const armed = armedDelivery === `${id}/${variable.id}`;
      const send = el('button', { type: 'button', class: armed ? 'danger' : 'secondary',
        textContent: armed ? t('Confirm delivery of {name}', { name: variable.name }) : t('Deliver {name}', { name: variable.name }) });
      send.onclick = () => busy(send, async () => {
        if (!armed) {
          armedDelivery = `${id}/${variable.id}`;
          clearTimeout(armTimer);
          armTimer = setTimeout(() => { armedDelivery = null; renderConsumers(); }, 8000);
          return renderConsumers();
        }
        armedDelivery = null;
        const reply = await invoke('variable_deliver', { destination: id, variable: variable.id, version: variable.version });
        const result = reply.result;
        consumerNote[`delivery:${id}`] = [t(DELIVERY_RESULTS[result.status] ?? DELIVERY_RESULTS.failed, { destination: id }),
          result.http_status ? t('HTTP {status}.', { status: result.http_status }) : '',
          result.confirmed_by_destination ? t('The destination confirmed the value it stored.') : ''].filter(Boolean).join(' ');
        renderConsumers();
      });
      actions.push(send);
    }
  }
  const remove = el('button', { type: 'button', class: 'danger', textContent: t('Remove'), 'aria-label': t('Remove destination {name}', { name: id }) });
  remove.onclick = () => busy(remove, async () => {
    await invoke('variable_delivery_remove', { id });
    await loadConsumers();
    done(t('Destination {name} removed. A value already delivered stays there until you rotate it at its provider.', { name: id }));
  });
  actions.push(remove);
  const note = consumerNote[`delivery:${id}`];
  return el('div', { class: 'row variable-row' },
    rowText(title, destination.description, status),
    ...deliveryLines(destination).map(line => el('p', { class: 'hint variable-note', textContent: line, translate: 'no' })),
    el('div', { class: 'variable-actions' }, actions),
    armedDelivery?.startsWith(`${id}/`) ? el('p', { class: 'hint variable-note', role: 'status', textContent: t('This host sends the value to the address above. It stays there until you rotate it at its provider.') }) : null,
    note ? el('p', { class: 'hint variable-note', role: 'status', textContent: note }) : null);
}

/** What reads a `[secrets]` reference: the sources and settings that name it. */
function referenceUses(reference) {
  const uses = [];
  for (const resource of current?.map?.resources ?? []) {
    const tool = resource.tool;
    if (!tool) continue;
    const kind = toolNames[tool.type] ?? tool.type;
    if (tool.secret_ref === reference) uses.push(t('{kind} source {id}', { kind, id: resource.id }));
    if (tool.link_secret_ref === reference) uses.push(t('linking work to work items in {id}', { id: resource.id }));
  }
  if (current?.config?.knowledge_index?.embeddings?.secret_ref === reference) uses.push(t('semantic search'));
  return uses;
}

/** One configured credential reference: which variable it uses by name, and a choice of another. */
function referenceRow(reference, name) {
  const key = `ref:${reference}`;
  const uses = referenceUses(reference);
  const title = el('span', { class: 'cred-name', textContent: reference, translate: 'no' });
  const using = variableState.variables.find(v => v.name.toUpperCase() === name.toUpperCase());
  const status = using ? badge(t('Using {name} · version {n}', { name: using.name, n: using.version }), 'ok')
    : badge(t('Missing {name}', { name }), 'warn');
  const summary = uses.length ? t('Used by {uses}.', { uses: uses.join(', ') }) : t('Not used by any source.');
  const chosen = consumerChoice[key] ?? using?.name ?? '';
  const select = el('select', { 'aria-label': t('Variable for {consumer}', { consumer: reference }) },
    el('option', { value: '', textContent: t('Choose a variable') }),
    variableState.variables.map(v => el('option', { value: v.name, textContent: `${v.name} · v${v.version}`, selected: v.name === chosen, translate: 'no' })));
  const use = el('button', { type: 'button', textContent: t('Use this variable') });
  const sync = () => { use.disabled = !select.value || select.value === name; };
  select.onchange = () => { consumerChoice[key] = select.value; sync(); };
  sync();
  use.onclick = () => busy(use, async () => {
    const chosenName = select.value;
    current.config.secrets[reference] = chosenName;
    try { await save(); } catch (error) { await reload(); throw error; }
    delete consumerChoice[key];
    consumerNote[key] = t('{consumer} now uses {name}.', { consumer: reference, name: chosenName });
    renderConsumers();
  });
  return el('div', { class: 'row variable-row' },
    rowText(title, summary, status),
    el('div', { class: 'variable-actions' }, [select, use]),
    consumerNote[key] ? el('p', { class: 'hint variable-note', role: 'status', textContent: consumerNote[key] }) : null);
}

/** Plain text only: summaries come from the host's fixed registry; names and paths are user data. */
function renderConsumers() {
  const { consumers, launchers, error } = consumerState;
  const list = $('#consumers');
  list.replaceChildren();
  if (error) list.append(el('p', { class: 'hint', role: 'status', textContent: tx(error) }));
  for (const consumer of consumers) list.append(consumerRow(consumer));
  for (const [reference, name] of Object.entries(current?.config?.secrets ?? {})) list.append(referenceRow(reference, name));
  $('#launchers').hidden = !launchers.length;
  $('#launcher-list').replaceChildren(...launchers.map(l => launcherRow(l, consumerState.launchersHere)));
  $('#deliveries').hidden = !deliveryState.destinations.length;
  $('#delivery-list').replaceChildren(...deliveryState.destinations.map(deliveryRow));
}

/** Plain text only: names and descriptions are user data, never markup or instructions. */
/** One line about where the catalog comes from and how current it is (metadata only, never a value). */
function syncSummary(sync) {
  if (!sync) return '';
  if (sync.mode === 'authoritative') return t('This computer holds your variables itself, so they are always current. Revision {n}.', { n: sync.revision });
  const loaded = sync.revision == null ? '' : t('Revision {n}, synchronized {when}.', { n: sync.revision, when: ago(sync.synced_at * 1000) });
  switch (sync.state) {
    case 'syncing': return t('Synchronizing…');
    case 'synced': return t('Synchronized with your account. {loaded}', { loaded });
    case 'conflict': return `${t('A change conflicted with a newer one. The list was refreshed: review it and try again.')} ${tx(sync.conflict ?? '')}`.trim();
    case 'error': return `${t('Synchronization failed; the last synchronized catalog is shown.')} ${loaded} ${tx(sync.error?.message ?? '')}`.trim();
    default: return t('Not synchronized yet.');
  }
}

function renderVariables() {
  const { variables, pending, error, sync } = variableState;
  const line = $('#variables-sync');
  line.textContent = syncSummary(sync);
  line.hidden = !line.textContent;
  const query = $('#variables-search').value.trim().toLowerCase();
  const shown = variables.filter(v => !query || v.name.toLowerCase().includes(query) || v.description.toLowerCase().includes(query));
  const list = $('#variables');
  list.replaceChildren();
  $('#variables-count').hidden = !variables.length;
  $('#variables-count').textContent = tn(variables.length, '{n} variable', '{n} variables');
  $('#variables-search-field').hidden = variables.length < 6;
  if (error) {
    list.append(el('p', { class: 'hint', role: 'status', textContent: tx(error) }));
  } else if (!variables.length) {
    list.append(el('p', { class: 'hint', textContent: t('No variables yet. Add one below, or ask your agent to request one.') }));
  } else if (!shown.length) {
    list.append(el('p', { class: 'hint', textContent: t('No variable matches this search.') }));
  }
  for (const variable of shown) list.append(variableRow(variable));
  renderPendingVariables(pending);
  updateVariableForm();
}

function variableRow(variable) {
  const { id, name } = variable;
  const title = el('span', { class: 'cred-name', textContent: name, translate: 'no' });
  const visible = revealedVariable?.id === id && revealedVariable.version === variable.version;
  const value = visible
    ? el('pre', { class: 'variable-value', textContent: revealedVariable.value, translate: 'no', tabIndex: 0, 'aria-label': t('Value of {name}', { name }) })
    : el('span', { class: 'variable-mask', textContent: '••••••••••', role: 'img', 'aria-label': t('Value hidden') });
  const reveal = el('button', { type: 'button', class: 'secondary', 'aria-pressed': String(visible),
    'aria-label': visible ? t('Hide the value of {name}', { name }) : t('Reveal the value of {name}', { name }) },
    icon(visible ? 'eye-off' : 'eye'), visible ? t('Hide') : t('Reveal'));
  reveal.onclick = () => busy(reveal, async () => {
    if (visible) return clearRevealedVariable();
    const epoch = accountEpoch, view = revealEpoch;
    const reply = await invoke('variable_reveal', { id });
    if (epoch !== accountEpoch || view !== revealEpoch || document.hidden) return;
    clearRevealedVariable(false);
    // The reply names the current version: show it against fresh metadata, never a stale row.
    variableState.variables = variableState.variables.map(v => v.id === id ? reply.variable : v);
    revealedVariable = { id, version: reply.variable.version, value: reply.value, timer: setTimeout(clearRevealedVariable, VALUE_SECONDS * 1000) };
    renderVariables();
  });
  const copyValue = el('button', { type: 'button', class: 'secondary', textContent: t('Copy value'), 'aria-label': t('Copy value of {name}', { name }) });
  copyValue.onclick = () => busy(copyValue, async () => {
    const epoch = accountEpoch, view = revealEpoch;
    const reply = await invoke('variable_reveal', { id });
    if (epoch !== accountEpoch || view !== revealEpoch || document.hidden) return;
    // Only the clipboard receives it: no toast, notice or log carries the value.
    if (!await copy(reply.value)) throw t('The value could not be copied. Reveal it and copy it by hand.');
    await scheduleClipboardCleanup(reply.value);
    done(t('Value of {name} copied. The clipboard is cleared in a minute if it is unchanged.', { name }));
  });
  const edit = el('button', { type: 'button', class: 'secondary', textContent: t('Edit'), 'aria-expanded': String(editingVariable === id), 'aria-label': t('Edit {name}', { name }) });
  edit.onclick = () => {
    editingVariable = editingVariable === id ? null : id;
    variableDraft = null;
    renderVariables();
  };
  const armed = deletingVariable === id;
  const remove = el('button', { type: 'button', class: 'danger', textContent: armed ? t('Confirm deletion') : t('Delete'),
    'aria-label': armed ? t('Confirm deletion of {name}', { name }) : t('Delete {name}', { name }) });
  remove.onclick = () => busy(remove, async () => {
    if (!armed) {
      // An irreversible action asks for a second tap, and disarms itself.
      deletingVariable = id;
      clearTimeout(armTimer);
      armTimer = setTimeout(() => { if (deletingVariable === id) { deletingVariable = null; renderVariables(); } }, 8000);
      return renderVariables();
    }
    deletingVariable = null;
    await changeVariable('variable_delete', variable);
    if (revealedVariable?.id === id) clearRevealedVariable(false);
    if (editingVariable === id) editingVariable = null;
    await loadVariables();
    done(t('{name} deleted.', { name }));
  });
  const actions = [reveal, copyValue, edit, remove];
  if (armed) {
    const keep = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel'), 'aria-label': t('Cancel deletion of {name}', { name }) });
    keep.onclick = () => { deletingVariable = null; renderVariables(); };
    actions.push(keep);
  }
  return el('div', { class: 'row variable-row' },
    rowText(title, variable.description, badge(t('Stored · version {n}', { n: variable.version }), 'ok')),
    el('div', { class: 'variable-value-box' }, value),
    el('div', { class: 'variable-actions' }, actions),
    armed ? el('p', { class: 'hint variable-note', role: 'status', textContent: t('Deleting removes it for good and its value cannot be recovered. It does not revoke the credential at its provider.') }) : null,
    editingVariable === id ? variableEditor(variable) : null);
}

/** Three independent edits of the same entry: name only, description only, and a replacement value only. */
function variableEditor(variable) {
  const { id, name } = variable;
  const draft = variableDraft?.id === id ? variableDraft : { id, name, description: variable.description };
  const keep = patch => { variableDraft = { ...(variableDraft?.id === id ? variableDraft : draft), ...patch }; };
  const newName = el('input', { value: draft.name, maxLength: 128, autocomplete: 'off', autocapitalize: 'off', spellcheck: false,
    translate: 'no', 'aria-label': t('New name of {name}', { name }) });
  const saveName = el('button', { type: 'button', textContent: t('Rename') });
  const syncName = () => { saveName.disabled = !newName.value.trim() || newName.value.trim() === name; };
  newName.oninput = () => { keep({ name: newName.value }); syncName(); };
  syncName();
  saveName.onclick = () => busy(saveName, async () => {
    const next = newName.value.trim();
    if (!VARIABLE_NAME.test(next)) throw t('A variable name uses letters, digits and _, cannot start with a digit and has at most 128 characters.');
    await changeVariable('variable_rename', variable, { name: next });
    variableDraft = editingVariable = null;
    await loadVariables();
    done(t('{name} renamed to {next}. Its value and references are unchanged.', { name, next }));
  });
  const description = el('input', { value: draft.description, maxLength: 500,
    autocomplete: 'off', spellcheck: false, 'aria-label': t('Description of {name}', { name }) });
  const saveDescription = el('button', { type: 'button', textContent: t('Save description') });
  const sync = () => { saveDescription.disabled = !description.value.trim() || description.value.trim() === variable.description; };
  description.oninput = () => { keep({ description: description.value }); sync(); };
  sync();
  saveDescription.onclick = () => busy(saveDescription, async () => {
    const text = description.value.trim();
    if (!text || text.length > 500 || UNSAFE_TEXT.test(text)) throw t('A description is plain text on one line, up to 500 characters.');
    await changeVariable('variable_update', variable, { description: text });
    variableDraft = editingVariable = null;
    await loadVariables();
    done(t('Description of {name} updated.', { name }));
  });

  const input = el('textarea', { class: 'secret-entry', rows: 2, autocomplete: 'off', autocapitalize: 'off', spellcheck: false,
    placeholder: t('Enter the new value'), 'aria-label': t('New value of {name}', { name }) });
  const show = el('input', { type: 'checkbox' });
  show.onchange = () => input.classList.toggle('shown', show.checked);
  const replace = el('button', { type: 'button', textContent: t('Replace value'), disabled: true });
  input.oninput = () => { replace.disabled = !input.value; };
  replace.onclick = () => busy(replace, async () => {
    checkVariableValue(input.value);
    const next = input.value;
    input.value = '';
    await changeVariable('variable_update', variable, { value: next });
    if (revealedVariable?.id === id) clearRevealedVariable(false);
    variableDraft = editingVariable = null;
    await loadVariables();
    done(t('Value of {name} replaced. Its name and description are unchanged.', { name }));
  });
  const close = el('button', { type: 'button', class: 'secondary', textContent: t('Cancel') });
  close.onclick = () => { input.value = ''; editingVariable = variableDraft = null; renderVariables(); };
  return el('div', { class: 'variable-edit' },
    el('label', {}, t('Name'), newName),
    el('div', { class: 'actions' }, saveName),
    el('label', {}, t('Description'), description),
    el('div', { class: 'actions' }, saveDescription),
    el('label', {}, t('New value'), el('small', { textContent: t('The name, description and references stay the same.') }), input),
    el('label', { class: 'check' }, show, icon('eye'), ` ${t('Show the value while I type')}`),
    el('div', { class: 'actions' }, replace, close));
}

function renderPendingVariables(pending) {
  const box = $('#variables-pending');
  box.hidden = !pending.length;
  box.replaceChildren();
  if (!pending.length) return;
  box.append(el('p', { class: 'hint', textContent: t('Your agent asked you to store these. Type each value here: the agent never sees it.') }));
  for (const entry of pending) {
    const title = el('span', { class: 'cred-name', textContent: entry.name, translate: 'no' });
    const input = el('textarea', { class: 'secret-entry', rows: 1, autocomplete: 'off', autocapitalize: 'off', spellcheck: false,
      placeholder: t('Enter the value'), 'aria-label': t('Value of {name}', { name: entry.name }) });
    const save = el('button', { textContent: t('Save'), disabled: true });
    const cancel = el('button', { class: 'secondary', textContent: t('Cancel'), 'aria-label': t('Cancel request for {name}', { name: entry.name }) });
    input.oninput = () => { save.disabled = !input.value; };
    save.onclick = () => busy(save, async () => {
      checkVariableValue(input.value);
      await invoke('variable_entry_complete', { request: entry.request, value: input.value });
      input.value = '';
      await loadVariables();
      done(t('{name} stored.', { name: entry.name }));
    });
    cancel.onclick = () => busy(cancel, async () => {
      await invoke('variable_entry_cancel', { request: entry.request });
      await loadVariables();
      done(t('Request for {name} cancelled.', { name: entry.name }));
    });
    box.append(el('div', { class: 'row' }, rowText(title, entry.description, badge(t('Waiting for you'), 'warn')), input, save, cancel));
  }
}

function checkVariableValue(value) {
  if (!value || value.includes('\u0000') || new TextEncoder().encode(value).length > 16384) throw t('A value has 1 byte to 16 KiB of text without NUL characters.');
}

function variableForm() {
  return { name: $('#variable-name').value.trim(), description: $('#variable-description').value.trim(), value: $('#variable-value').value };
}
function updateVariableForm() {
  const form = variableForm();
  $('#variable-create').disabled = !form.name || !form.description || !form.value;
}

$('#variables-search').oninput = renderVariables;
for (const id of ['#variable-name', '#variable-description', '#variable-value']) $(id).oninput = updateVariableForm;
$('#variable-show').onchange = event => $('#variable-value').classList.toggle('shown', event.target.checked);
$('#variable-create').onclick = event => busy(event.currentTarget, async () => {
  const form = variableForm();
  if (!VARIABLE_NAME.test(form.name)) throw t('A variable name uses letters, digits and _, cannot start with a digit and has at most 128 characters.');
  if (form.description.length > 500 || UNSAFE_TEXT.test(form.description)) throw t('A description is plain text on one line, up to 500 characters.');
  checkVariableValue(form.value);
  const fingerprint = JSON.stringify([form.name, form.description, form.value]);
  if (variableAttempt?.fingerprint !== fingerprint) variableAttempt = { id: crypto.randomUUID(), fingerprint };
  const reply = await invoke('variable_create', { id: variableAttempt.id, ...form });
  variableAttempt = null;
  for (const id of ['#variable-name', '#variable-description', '#variable-value']) $(id).value = '';
  await loadVariables();
  done(reply.replayed ? t('{name} was already stored.', { name: form.name }) : t('{name} stored.', { name: form.name }));
});
$('#variables-refresh').onclick = event => busy(event.currentTarget, async () => {
  // Rereads this computer's list; bringing the account's catalog here is the sidebar's Synchronize.
  await loadVariables();
  done(t('Variables refreshed.'));
});
// The periodic refresh never rebuilds a form the user is typing in.
const typingVariable = () => editingVariable || document.activeElement?.closest?.('#consumers') || [...document.querySelectorAll('#variables-pending textarea')].some(field => field.value);
setInterval(() => { if (tab === 'settings' && !$('#panel-variables').closest('[role=tabpanel]').hidden && !typingVariable()) loadVariables(); }, 10_000);

/** @param {any} value */
function setVariableState(value) { variableState = value; }
/** @param {any} value */
function setVariableAttempt(value) { variableAttempt = value; }
/** @param {any} value */
function setConsumerState(value) { consumerState = value; }

export {
  variableState, variableAttempt, clearRevealedVariable, clearTypedVariableValues, loadVariables, consumerState,
  consumerChoice, consumerNote, loadConsumers, renderConsumers, renderVariables, setConsumerState,
  setVariableAttempt, setVariableState,
};
