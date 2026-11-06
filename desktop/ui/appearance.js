// This device's look (Settings › General › Appearance): mode, palette and style, kept in
// localStorage and applied before the GUI renders.

/** This device's look (Settings › General › Appearance): the mode (`light`, `dark` or none to follow
 *  the system), the palette and the style. They live in `localStorage`, not in the profile, and apply
 *  before the first paint; an unknown or missing value is the default (Paper, Stickers, the system). */
const THEME_KEY = 'pta-theme';
const PALETTE_KEY = 'pta-palette';
const STYLE_KEY = 'pta-style';
const DEFAULT_PALETTE = 'paper';
const DEFAULT_STYLE = 'sticker';
function storedLook(key) { try { return localStorage.getItem(key); } catch { return null; } }
function storeLook(key, value) {
  try { if (value) localStorage.setItem(key, value); else localStorage.removeItem(key); }
  catch { /* applies to this page only */ }
}
function applyAppearance(look = {}) {
  const root = document.documentElement;
  const offered = (group, attribute, value) => Boolean(value) && [...document.querySelectorAll(`${group} button`)].some(button => button.dataset[attribute] === value);
  const theme = ['light', 'dark'].includes(look.theme) ? look.theme : null;
  const palette = offered('#palette-choice', 'palette', look.palette) ? look.palette : DEFAULT_PALETTE;
  const style = offered('#style-choice', 'style', look.style) ? look.style : DEFAULT_STYLE;
  // The defaults carry no attribute: style.css draws them from `:root` alone.
  const set = (name, value) => { if (value) root.dataset[name] = value; else delete root.dataset[name]; };
  set('theme', theme);
  set('palette', palette === DEFAULT_PALETTE ? null : palette);
  set('style', style === DEFAULT_STYLE ? null : style);
  const press = (group, attribute, value) => {
    for (const button of document.querySelectorAll(`${group} button`)) button.setAttribute('aria-pressed', String(button.dataset[attribute] === value));
  };
  press('#theme-choice', 'themeChoice', theme ?? 'system');
  press('#palette-choice', 'palette', palette);
  press('#style-choice', 'style', style);
  // The browser's bar takes the page's color: the chosen look's, or each mode's under the defaults.
  const page = getComputedStyle(root).backgroundColor;
  for (const meta of document.querySelectorAll('meta[name="theme-color"]')) {
    meta.dataset.color ??= meta.content;
    meta.content = theme || palette !== DEFAULT_PALETTE ? page : meta.dataset.color;
  }
}
/** The look in use; where `localStorage` is unavailable a choice still holds for this page. */
const look = { theme: storedLook(THEME_KEY), palette: storedLook(PALETTE_KEY), style: storedLook(STYLE_KEY) };
applyAppearance(look);
// A palette that follows the system changes its page color with it.
window.matchMedia?.('(prefers-color-scheme: dark)').addEventListener?.('change', () => applyAppearance(look));
// Each choice is saved on this device and applied at once; the rest of the look stays as it was.
function chooseLook(key, part, value) {
  storeLook(key, value);
  look[part] = value;
  applyAppearance(look);
}

export { THEME_KEY, PALETTE_KEY, STYLE_KEY, DEFAULT_PALETTE, DEFAULT_STYLE, applyAppearance, chooseLook };
