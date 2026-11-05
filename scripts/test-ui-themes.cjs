// Every palette, in light and dark and under every style, keeps the contrast the GUI conventions
// ask for (WCAG 2.2 AA): 4.5:1 for text on each surface and fill, 3:1 for control boundaries.
// The tokens are resolved from style.css the way the browser would: var(), light-dark() and
// color-mix(in srgb).
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const ui = path.join(__dirname, '../desktop/ui');
const css = fs.readFileSync(path.join(ui, 'style.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const html = fs.readFileSync(path.join(ui, 'index.html'), 'utf8');

/** Top-level rules as [selectors, declarations]; at-rules are skipped whole. */
function rules() {
  const found = [];
  let depth = 0, start = 0, selector = '', skip = false;
  for (let i = 0; i < css.length; i++) {
    if (css[i] === '{') {
      if (depth === 0) { selector = css.slice(start, i).trim(); skip = selector.startsWith('@'); start = i + 1; }
      depth++;
    } else if (css[i] === '}') {
      depth--;
      if (depth === 0) { if (!skip) found.push([selector.split(',').map(s => s.trim()), css.slice(start, i)]); start = i + 1; }
    }
  }
  return found;
}
const allRules = rules();

/** Custom properties of the root element for one palette and style, in cascade order. */
function tokens(palette, style) {
  const matches = selector => {
    if (selector === ':root') return 1;
    let rest = selector.startsWith(':root') ? selector.slice(5) : selector;
    let specificity = selector.startsWith(':root') ? 1 : 0;
    const attrs = { palette, style };
    for (const part of rest.match(/:not\(\[[^\]]+\]\)|\[[^\]]+\]/g) ?? []) {
      const negated = part.startsWith(':not(');
      const m = part.match(/\[data-(palette|style)(?:="([^"]+)")?\]/);
      if (!m) return 0;
      const has = m[2] === undefined ? attrs[m[1]] !== null : attrs[m[1]] === m[2];
      if (has === negated) return 0;
      specificity++;
      rest = rest.replace(part, '');
    }
    return rest.trim() === '' ? specificity : 0;
  };
  const found = [];
  allRules.forEach(([selectors, body], order) => {
    const specificity = Math.max(...selectors.map(matches));
    if (specificity) found.push({ specificity, order, body });
  });
  found.sort((a, b) => a.specificity - b.specificity || a.order - b.order);
  const values = {};
  for (const { body } of found) for (const m of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) values[m[1]] = m[2].trim();
  return values;
}

/** Splits `a, b(c, d), e` at its top-level commas. */
function split(text) {
  const parts = [];
  let depth = 0, start = 0;
  for (let i = 0; i < text.length; i++) {
    if (text[i] === '(') depth++;
    else if (text[i] === ')') depth--;
    else if (text[i] === ',' && depth === 0) { parts.push(text.slice(start, i).trim()); start = i + 1; }
  }
  parts.push(text.slice(start).trim());
  return parts;
}
function color(text, values, dark) {
  text = text.trim();
  let m;
  if ((m = text.match(/^var\((--[\w-]+)(?:\s*,\s*(.+))?\)$/))) {
    if (values[m[1]] === undefined && m[2] === undefined) throw new Error(`${m[1]} is not defined`);
    return color(values[m[1]] ?? m[2], values, dark);
  }
  if ((m = text.match(/^#([0-9a-f]{6})$/i))) return [0, 2, 4].map(i => parseInt(m[1].slice(i, i + 2), 16));
  if ((m = text.match(/^light-dark\((.+)\)$/))) return color(split(m[1])[dark ? 1 : 0], values, dark);
  if ((m = text.match(/^color-mix\(in srgb,\s*(.+)\)$/))) {
    const [first, second] = split(m[1]);
    const share = first.match(/^(.+)\s+(\S+)$/);
    const amount = number(share[2], values) / 100;
    const a = color(share[1], values, dark), b = color(second, values, dark);
    return a.map((channel, i) => channel * amount + b[i] * (1 - amount));
  }
  throw new Error(`cannot read the color ${text}`);
}
function number(text, values) {
  const m = text.match(/^var\((--[\w-]+)\)$/);
  return parseFloat(m ? values[m[1]] : text);
}
const luminance = rgb => rgb.map(c => c / 255).map(c => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  .reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
function contrast(a, b) {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05);
}

const palettes = [...css.matchAll(/\[data-palette="([\w-]+)"\] \{/g)].map(m => m[1]);
const styles = [...new Set([...css.matchAll(/\[data-style="([\w-]+)"\]/g)].map(m => m[1]))];
const states = ['ok', 'warn', 'error', 'info', 'active'];
const sections = ['overview', 'messages', 'registration', 'knowledge', 'chat', 'settings', 'todo'];
/** [foreground, background, minimum]: what the components put on what. */
const pairs = [
  ...['--bg', '--surface', '--surface-2', '--sunken'].map(surface => ['--text', surface, 4.5]),
  ...['--bg', '--surface', '--surface-2'].flatMap(surface => [['--muted', surface, 4.5], ['--link', surface, 4.5]]),
  ...states.flatMap(state => [
    ['--on-color', `--${state}-fill`, 4.5], ['--text', `--${state}-soft`, 4.5], ['--muted', `--${state}-soft`, 4.5],
    [`--${state}`, '--bg', 4.5], [`--${state}`, '--surface', 4.5], [`--${state}`, '--surface-2', 4.5], [`--${state}`, `--${state}-soft`, 4.5],
  ]),
  ['--text', '--muted-soft', 4.5], ['--muted', '--muted-soft', 4.5],
  ...sections.map(section => ['--on-color', `--c-${section}`, 4.5]),
  ['--on-primary', '--primary', 4.5], ['--inverse-text', '--inverse', 4.5], ['--bar-text', '--bar', 4.5],
  ['--edge', '--surface', 3], ['--edge', '--bg', 3], ['--focus', '--bg', 3], ['--focus', '--surface', 3],
];

test('the choices in Settings are the palettes and styles of style.css', () => {
  const offered = attr => [...html.matchAll(new RegExp(`<button[^>]* data-${attr}="([\\w-]+)"`, 'g'))].map(m => m[1]);
  assert.deepEqual(offered('palette').sort(), [...palettes].sort());
  assert.deepEqual(offered('style').sort(), [...styles].sort());
  assert.equal(palettes.length, 10);
  assert.equal(styles.length, 3);
});

for (const palette of palettes) {
  for (const style of styles) {
    for (const dark of [false, true]) {
      test(`${palette} · ${style} · ${dark ? 'dark' : 'light'} keeps its contrast`, () => {
        const values = tokens(palette === 'paper' ? null : palette, style === 'sticker' ? null : style);
        const failures = [];
        for (const [front, back, minimum] of pairs) {
          const ratio = contrast(color(`var(${front})`, values, dark), color(`var(${back})`, values, dark));
          if (ratio < minimum) failures.push(`${front} on ${back}: ${ratio.toFixed(2)} < ${minimum}`);
        }
        // What a tinted tile or hero carries: the fill at the style's strength over the surface.
        for (const fill of [...states.map(s => `--${s}-fill`), ...sections.map(s => `--c-${s}`)]) {
          const ratio = contrast(color('var(--on-fill)', values, dark), color(`color-mix(in srgb, var(${fill}) var(--fill-strength), var(--surface))`, values, dark));
          if (ratio < 4.5) failures.push(`--on-fill on ${fill} at ${values['--fill-strength']}: ${ratio.toFixed(2)} < 4.5`);
        }
        assert.deepEqual(failures, []);
      });
    }
  }
}
