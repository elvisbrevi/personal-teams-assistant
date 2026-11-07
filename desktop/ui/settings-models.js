// Settings › Models: the model chain and embeddings.
import { t, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { $, el, field } from './dom.js';
import { current, live } from './state.js';
import { fail } from './notices.js';
import { markDirty } from './nav.js';
import { providerNames } from './home.js';

const effortNames = { default: t('Model default'), none: t('No reasoning'), minimal: t('Minimal'), low: t('Low'), medium: t('Medium'), high: t('High'), xhigh: t('Very high'), max: t('Maximum') };
let llmCatalog = { providers: [] };
let llmRows = [];

/** Keep each configured model in priority order, including repeated providers. */
function chainRows(config) {
  return config.llm.chain.length
    ? config.llm.chain.map(choice => ({ ...choice }))
    : [{ provider: config.llm.provider, model: config.llm.model, effort: 'max', enabled: true }];
}

function providerStatus(info) {
  if (!info) return t('Provider not available in this version');
  if (info.transport === 'cli') return info.available ? `${t('CLI installed')}${info.version ? ' · ' + info.version : ''}` : t('CLI not installed on this computer');
  const source = live.credentials[info.credential];
  return source && source !== 'missing' && source !== 'unused' ? t('API · {name} set', { name: info.credential }) : t('API · {name} missing in Variables', { name: info.credential });
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
  const providerSelect = $('#add-llm-provider');
  const selected = providerSelect.value;
  providerSelect.replaceChildren(...llmCatalog.providers.map(info => new Option(providerNames[info.id] || info.id, info.id)));
  if (selected) providerSelect.value = selected;
  const addedProvider = llmCatalog.providers.find(info => info.id === providerSelect.value);
  $('#add-llm').disabled = llmRows.length >= 16 || !addedProvider || (addedProvider.id === 'opencode' && !addedProvider.models.length);
  providerSelect.onchange = renderChain;
  let active = 0;
  llmRows.forEach((row, index) => {
    const info = llmCatalog.providers.find(provider => provider.id === row.provider);
    const name = providerNames[row.provider] || row.provider;
    const rank = el('span', { class: 'rank', textContent: !row.enabled ? t('Off') : active++ === 0 ? t('Default') : t('Fallback {n}', { n: active - 1 }) });
    const toggle = el('input', { type: 'checkbox', checked: row.enabled, disabled: !row.enabled && !(info?.available), 'aria-label': t('Turn on {name}', { name }) });
    toggle.onchange = () => { row.enabled = toggle.checked; markDirty({ target: list }); renderChain(); };
    const label = el('label', { class: 'check' }, toggle,
      el('div', {}, el('strong', { textContent: name }), el('small', { textContent: providerStatus(info) })));
    const models = (info?.models ?? []).map(model => [model.id, model.name]);
    const openCode = row.provider === 'opencode';
    const knownModel = models.some(([id]) => id === row.model);
    if (row.model && !knownModel && !openCode) models.unshift([row.model, `${row.model} (${t('current')})`]);
    const [modelField, model] = select(t('Model'), t('{name} model', { name }), models, row.model);
    if (openCode && !knownModel) {
      model.prepend(el('option', { value: '', textContent: t('Select an OpenCode favorite'), disabled: true, selected: true }));
      modelField.append(el('small', { textContent: t('Saved model: {model}. It is not in the available favorites.', { model: row.model }) }));
    }
    model.disabled = openCode && !models.length;
    model.title = row.model;
    const efforts = () => info?.models.find(m => m.id === row.model)?.efforts ?? (openCode ? [] : info?.efforts ?? [row.effort]);
    const effortOptions = () => {
      const values = efforts();
      return (openCode || values.includes(row.effort) ? values : [row.effort, ...values]).map(value => [value, openCode && value !== 'default' ? value : effortNames[value] || value]);
    };
    const [effortField, effort] = select(t('Effort'), t('{name} effort', { name }), effortOptions(), row.effort);
    if (openCode && !efforts().includes(row.effort)) {
      effort.prepend(el('option', { value: '', textContent: t('Select a supported variant'), disabled: true, selected: true }));
      effortField.append(el('small', { textContent: t('Saved effort: {effort}.', { effort: row.effort }) }));
    }
    effort.disabled = openCode && !efforts().length;
    model.onchange = () => {
      row.model = model.value;
      const supported = efforts();
      if (!supported.includes(row.effort)) row.effort = info?.models.find(m => m.id === row.model)?.default_effort ?? supported[0];
      markDirty({ target: list });
      renderChain();
    };
    effort.onchange = () => { row.effort = effort.value; markDirty({ target: list }); };
    const order = el('div', { class: 'order' });
    for (const [text, offset, aria] of /** @type {[string, number, string][]} */ ([['↑', -1, t('Raise the priority of {name}', { name })], ['↓', 1, t('Lower the priority of {name}', { name })]])) {
      const button = el('button', { class: 'secondary', textContent: text, 'aria-label': aria, disabled: !llmRows[index + offset] });
      button.onclick = () => {
        [llmRows[index], llmRows[index + offset]] = [llmRows[index + offset], llmRows[index]];
        markDirty({ target: list });
        renderChain();
      };
      order.append(button);
    }
    const remove = el('button', { class: 'secondary small', textContent: t('Remove'), 'aria-label': t('Remove {name} model', { name }) });
    remove.onclick = () => { llmRows.splice(index, 1); markDirty({ target: list }); renderChain(); };
    order.append(remove);
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

/** The reference the embeddings credential is mapped from in `[secrets]`. */
const EMBEDDINGS_SECRET = 'secret://knowledge/embeddings';

function collectEmbeddings() {
  const config = current.config;
  const url = $('#embeddings-url').value.trim();
  const model = $('#embeddings-model').value.trim();
  const credential = $('#embeddings-credential').value.trim();
  if (!url && !model && !credential) {
    delete config.knowledge_index;
    delete config.secrets[EMBEDDINGS_SECRET];
    return;
  }
  if (!url || !model) throw t('Semantic search needs both the endpoint URL and the model, or neither.');
  if (credential && !/^[A-Z][A-Z0-9_]{0,63}$/.test(credential)) throw t('The credential name uses capital letters, digits and _, like OPENAI_API_KEY.');
  if (credential) config.secrets[EMBEDDINGS_SECRET] = credential;
  else delete config.secrets[EMBEDDINGS_SECRET];
  config.knowledge_index = { embeddings: { url, model, ...(credential ? { secret_ref: EMBEDDINGS_SECRET } : {}) } };
}

function renderEmbeddings() {
  const embeddings = current.config.knowledge_index?.embeddings;
  field('#embeddings-url', embeddings?.url);
  field('#embeddings-model', embeddings?.model);
  field('#embeddings-credential', embeddings?.secret_ref ? current.config.secrets[embeddings.secret_ref] : '');
}

/** @param {any} value */
function setLlmCatalog(value) { llmCatalog = value; }

/**
 * Fills the CLI, model and effort selects of a coding agent from a machine's catalog
 * (`implementation_catalog` providers), keeping `chosen` where the catalog lists it. A choice the
 * catalog does not list (an offline machine) stays as the only option: the host checks it again.
 * `changed(agent, byUser)` hears every value the selects settle on.
 */
function fillAgentChoices({ provider, model, effort }, providers, chosen, changed) {
  const list = providers.length ? providers
    : chosen.provider ? [{ id: chosen.provider, models: [{ id: chosen.model, name: chosen.model, efforts: [chosen.effort] }] }] : [];
  provider.replaceChildren(...list.map(p => el('option', { value: p.id, textContent: providerNames[p.id] || p.id })));
  if (!list.length) provider.append(el('option', { value: '', textContent: t('No coding CLI installed on this machine') }));
  provider.value = list.some(p => p.id === chosen.provider) ? chosen.provider : list[0]?.id ?? '';
  const agent = () => ({ provider: provider.value, model: model.value, effort: effort.value });
  const efforts = () => {
    const info = list.find(p => p.id === provider.value)?.models?.find(m => m.id === model.value);
    effort.replaceChildren(...(info?.efforts ?? []).map(value => el('option', { value, textContent: effortNames[value] || value })));
    effort.value = info?.efforts?.includes(chosen.effort) ? chosen.effort : info?.default_effort ?? info?.efforts?.[0] ?? '';
  };
  const models = () => {
    const info = list.find(p => p.id === provider.value);
    model.replaceChildren(...(info?.models ?? []).map(m => el('option', { value: m.id, textContent: m.name || m.id })));
    model.value = info?.models?.some(m => m.id === chosen.model) ? chosen.model : info?.models?.[0]?.id ?? '';
    efforts();
  };
  provider.onchange = () => { models(); changed(agent(), true); };
  model.onchange = () => { efforts(); changed(agent(), true); };
  effort.onchange = () => changed(agent(), true);
  models();
  changed(agent(), false);
}

export {
  effortNames, llmCatalog, llmRows, renderChain, loadChain, loadSelfChat, collectEmbeddings, renderEmbeddings,
  setLlmCatalog, fillAgentChoices,
};
