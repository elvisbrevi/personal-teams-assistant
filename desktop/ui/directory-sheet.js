// The shared directory sheet (`chooseDirectories`): every directory choice of the GUI.
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

export { chooseDirectories };
