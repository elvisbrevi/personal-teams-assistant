// The shared sheets: `chooseDirectories`, every directory choice of the GUI, and
// `chooseFromList`, a searchable sheet over a list (a workflow's project, repositories, chat).
import { t, tn, tx } from './i18n.js';
import { el, icon } from './dom.js';

/** Shared directory sheet: selections are staged until Apply, including across navigation. */
function chooseDirectories({ trigger, title, selected = [], multiple = true, allowEmpty = false, browse, apply }) {
  const selection = new Set(selected);
  const dialog = el('dialog', { class: 'directory-picker', 'aria-label': title });
  const back = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Parent directory') }, icon('back'));
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Cancel') }, icon('close'));
  const body = el('div', { class: 'directory-picker-body' });
  const count = el('span', { class: 'hint', role: 'status' });
  const use = el('button', { type: 'button', textContent: t('Use selected directories') });
  let request = 0;
  const update = () => {
    count.textContent = tn(selection.size, '{n} directory selected', '{n} directories selected');
    use.disabled = !allowEmpty && !selection.size;
  };
  const row = (path, current = false) => {
    const check = el('input', { type: 'checkbox', checked: selection.has(path), 'aria-label': t('Select directory {path}', { path }) });
    check.onchange = () => {
      if (!multiple) { selection.clear(); for (const input of body.querySelectorAll('input')) if (input !== check) input.checked = false; }
      if (check.checked) selection.add(path); else selection.delete(path);
      update();
    };
    const name = el('span', { textContent: current ? t('Select this directory') : path, title: path });
    const open = current ? null : el('button', { type: 'button', class: 'link-button', 'aria-label': t('Browse directory {path}', { path }) }, name);
    if (!current) open.onclick = () => load(path);
    return el('div', { class: 'directory-picker-row' }, el('label', { class: 'check' }, check, current ? name : icon('folder')), open);
  };
  const load = async path => {
    const generation = ++request;
    back.disabled = true;
    body.setAttribute('aria-busy', 'true');
    body.replaceChildren(el('p', { class: 'hint', role: 'status', textContent: t('Loading…') }));
    try {
      const result = await browse(path);
      if (!dialog.open || generation !== request) return;
      back.disabled = Boolean(result.roots);
      back.onclick = () => load(result.parent || '');
      body.replaceChildren(...[el('p', { class: 'directory-picker-location', textContent: result.path || t('Allowed roots') }),
        el('div', { class: 'directory-picker-list' },
          result.path ? el('div', { class: 'directory-picker-row' }, el('button', { type: 'button', class: 'link-button directory-picker-parent', translate: false, textContent: '../', 'aria-label': t('Parent directory'), onclick: () => load(result.parent || '') })) : null,
          result.path ? row(result.path, true) : null, ...result.directories.map(path => row(path))),
        result.directories.length ? null : el('p', { class: 'hint', textContent: t('No subdirectories.') })].filter(Boolean));
    } catch (error) {
      if (!dialog.open || generation !== request) return;
      const retry = el('button', { type: 'button', class: 'secondary', textContent: t('Retry') });
      retry.onclick = () => load(path);
      back.disabled = !path;
      back.onclick = () => load('');
      body.replaceChildren(el('p', { class: 'callout error', role: 'alert', textContent: tx(String(error?.message ?? error)) }), retry);
    } finally {
      if (dialog.open && generation === request) { body.setAttribute('aria-busy', 'false'); update(); }
    }
  };
  close.onclick = () => dialog.close();
  use.onclick = () => { apply([...selection]); dialog.close(); };
  dialog.onclose = () => { ++request; dialog.remove(); document.body.classList.toggle('directory-picker-open', Boolean(document.querySelector('dialog[open]'))); trigger?.focus({ preventScroll: true }); };
  dialog.append(el('div', { class: 'directory-picker-head' }, back, el('h2', { textContent: title }), close), body,
    el('div', { class: 'directory-picker-footer' }, count, use));
  document.body.append(dialog);
  document.body.classList.add('directory-picker-open');
  update(); dialog.showModal(); close.focus(); load('');
}

/** Names compared without letter case or accents. */
const normalize = text => String(text ?? '').normalize('NFD').replace(/[\u0300-\u036f]/g, '').toLocaleLowerCase();

/**
 * A searchable modal sheet over a list: a name filter, up to three recent choices, then the
 * rest. One choice applies at once; several are staged until the footer's confirmation.
 * Escape and Cancel keep the previous choice, and closing returns focus to the trigger.
 */
function chooseFromList({ trigger, title, items, selected = [], recent = [], multiple = false, max = Infinity, allLabel, empty, confirm = '', countText, apply }) {
  const chosen = new Set(selected.filter(v => items.some(i => i.value === v)));
  const dialog = el('dialog', { class: 'directory-picker destination-picker', 'aria-label': title });
  const close = el('button', { type: 'button', class: 'icon-button secondary', 'aria-label': t('Cancel') }, icon('close'));
  const search = el('input', { type: 'search', autocomplete: 'off' });
  const body = el('div', { class: 'directory-picker-body' });
  const count = el('p', { class: 'hint', role: 'status' });
  const use = multiple ? el('button', { type: 'button', textContent: confirm }) : null;
  const firstRecent = recent.filter((v, i) => recent.indexOf(v) === i && items.some(item => item.value === v)).slice(0, 3);
  const update = shown => {
    count.textContent = countText(multiple ? chosen.size : shown);
    if (use) use.disabled = chosen.size > max;
  };
  const render = () => {
    const query = normalize(search.value.trim());
    const matches = items.filter(item => normalize(item.label).includes(query));
    const row = item => {
      if (multiple) {
        const box = el('input', { type: 'checkbox', checked: chosen.has(item.value) });
        box.onchange = () => { if (box.checked) chosen.add(item.value); else chosen.delete(item.value); update(matches.length); };
        return el('div', { class: 'directory-picker-row' }, el('label', { class: 'check' }, box,
          el('span', {}, item.label, item.detail ? el('small', { textContent: ` · ${item.detail}` }) : null)));
      }
      const button = el('button', { type: 'button', class: 'link-button', textContent: item.label, 'aria-pressed': String(chosen.has(item.value)) });
      button.onclick = () => { dialog.close(); apply([item.value]); };
      return el('div', { class: 'directory-picker-row' }, button);
    };
    const recentMatches = firstRecent.map(v => matches.find(m => m.value === v)).filter(Boolean);
    const others = matches.filter(m => !recentMatches.includes(m));
    body.replaceChildren(...[
      recentMatches.length ? el('h3', { textContent: t('Recent') }) : null,
      recentMatches.length ? el('div', { class: 'directory-picker-list' }, ...recentMatches.map(row)) : null,
      others.length ? el('h3', { textContent: allLabel }) : null,
      others.length ? el('div', { class: 'directory-picker-list' }, ...others.map(row)) : null,
      !matches.length ? el('p', { class: 'hint', textContent: empty }) : null,
    ].filter(Boolean));
    update(matches.length);
  };
  search.oninput = render;
  dialog.onkeydown = event => { if (event.key === 'Escape') { event.preventDefault(); dialog.close(); } };
  close.onclick = () => dialog.close();
  if (use) use.onclick = () => { const values = [...chosen]; dialog.close(); apply(values); };
  dialog.onclose = () => {
    dialog.remove();
    document.body.classList.toggle('directory-picker-open', Boolean(document.querySelector('dialog[open]')));
    trigger?.focus({ preventScroll: true });
  };
  dialog.append(el('div', { class: 'directory-picker-head' }, el('h2', { textContent: title }), close),
    el('label', { class: 'destination-search' }, t('Filter by name'), search), body,
    el('div', { class: 'directory-picker-footer' }, count, use));
  document.body.append(dialog); document.body.classList.add('directory-picker-open');
  render(); dialog.showModal(); search.focus();
  return dialog;
}

export { chooseDirectories, chooseFromList, normalize };
