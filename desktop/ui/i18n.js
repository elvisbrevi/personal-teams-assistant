// Interface language. The English text in index.html and the GUI's modules is the source and the key;
// `llm.language` picks the language the user reads (Spanish by default). Host messages arrive in
// English, the language of the IPC contract and the CLI, and `tx` translates the known ones.
import { UI_CATALOG, UI_PATTERNS } from './catalog.js';

/** The interface's languages; English is the source text. @type {string[]} */
const UI_LANGUAGES = ['es', 'en'];

/** The language the user reads. @type {string} */
let uiLanguage = (() => {
  /** @type {string | null} */
  let stored = null;
  try { stored = localStorage.getItem('pta-language'); } catch { /* storage unavailable */ }
  if (stored && UI_LANGUAGES.includes(stored)) return stored;
  return typeof navigator !== 'undefined' && navigator.language?.toLowerCase().startsWith('en') ? 'en' : 'es';
})();

/** Locale for dates and numbers: the device's own variant when it speaks the same language. */
function uiLocale() {
  const device = typeof navigator !== 'undefined' ? navigator.language : '';
  return device?.toLowerCase().startsWith(uiLanguage) ? device : uiLanguage;
}

/**
 * `text` (English, the key) in the interface language, with `{name}` replaced from `vars`.
 * @param {string} text
 * @param {Record<string, unknown>} [vars]
 * @returns {string}
 */
function t(text, vars = {}) {
  const translated = UI_CATALOG[uiLanguage]?.[text] ?? text;
  return translated.replace(/\{(\w+)\}/g, (whole, name) => (name in vars ? String(vars[name]) : whole));
}
/**
 * One of two keys by count, with `{n}`.
 * @param {number} count
 * @param {string} one
 * @param {string} other
 * @param {Record<string, unknown>} [vars]
 * @returns {string}
 */
function tn(count, one, other, vars = {}) {
  return t(count === 1 ? one : other, { n: count, ...vars });
}
/**
 * A host message: translated when it is a known one, otherwise unchanged.
 * @param {string} text
 * @returns {string}
 */
function tx(text) {
  const exact = UI_CATALOG[uiLanguage]?.[text];
  if (exact) return exact;
  for (const [pattern, replacement] of UI_PATTERNS[uiLanguage] ?? []) {
    if (pattern.test(text)) return text.replace(pattern, replacement);
  }
  return text;
}
/**
 * A number in the interface locale, e.g. «1,5» in Spanish.
 * @param {number} value
 * @returns {string}
 */
function num(value, maximumFractionDigits = 2, minimumFractionDigits = 0) {
  return new Intl.NumberFormat(uiLocale(), { maximumFractionDigits, minimumFractionDigits }).format(value);
}

/**
 * Applies the configured language; a change reloads the page once. Returns whether it reloads.
 * @param {string} language
 * @returns {boolean}
 */
function useLanguage(language) {
  const next = UI_LANGUAGES.includes(language) ? language : 'es';
  let stored = false;
  try { localStorage.setItem('pta-language', next); stored = localStorage.getItem('pta-language') === next; } catch { /* not persisted */ }
  // Without storage a reload would guess the same language again: stay instead of looping.
  if (next === uiLanguage || !stored) return false;
  window.location.reload();
  return true;
}

/**
 * Translates the static page: text, placeholders, titles and labels; code and `translate="no"` stay.
 * @param {any} node A text node or an element (and, recursively, what it holds).
 */
function translatePage(node) {
  if (node.nodeType === 3) {
    const key = node.nodeValue.replace(/\s+/g, ' ').trim();
    const value = key && t(key);
    if (value && value !== key) node.nodeValue = node.nodeValue.replace(/\S[\s\S]*\S|\S/, () => value);
    return;
  }
  if (node.nodeType !== 1 || ['CODE', 'PRE', 'SCRIPT', 'STYLE'].includes(node.tagName)
    || node.namespaceURI === 'http://www.w3.org/2000/svg' || node.getAttribute('translate') === 'no') return;
  for (const name of ['placeholder', 'title', 'aria-label']) {
    const value = node.getAttribute(name);
    if (value) node.setAttribute(name, t(value));
  }
  for (const child of node.childNodes) translatePage(child);
}

if (typeof document !== 'undefined' && document.body) {
  document.documentElement.lang = uiLanguage;
  if (uiLanguage !== 'en') translatePage(document.body);
}

export { UI_LANGUAGES, uiLanguage, uiLocale, t, tn, tx, num, useLanguage, translatePage };
