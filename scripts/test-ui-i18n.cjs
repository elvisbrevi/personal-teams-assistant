// Every string the interface shows has a Spanish translation, with the same placeholders.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const ui = path.join(__dirname, '../desktop/ui');
const app = fs.readFileSync(path.join(ui, 'app.js'), 'utf8');
const html = fs.readFileSync(path.join(ui, 'index.html'), 'utf8');
const i18n = fs.readFileSync(path.join(ui, 'i18n.js'), 'utf8');

function load(language, device = 'es-CL') {
  const context = vm.createContext({ localStorage: { getItem: () => language, setItem() {} }, navigator: { language: device }, Intl });
  vm.runInContext(`${i18n}\nthis.catalog = UI_CATALOG;`, context);
  return context;
}

const unescape = text => text.replace(/\\(.)/g, '$1');
const literal = String.raw`(['"\`])((?:\\.|(?!\1)[^\\])*)\1`;
/** Keys passed as literals to t() and tn() in app.js. */
function scriptKeys() {
  const keys = [];
  for (const m of app.matchAll(new RegExp(String.raw`\bt\(\s*${literal}`, 'g'))) keys.push(unescape(m[2]));
  const second = literal.replace('\\1', '\\3').replace('(?!\\1)', '(?!\\3)');
  for (const m of app.matchAll(new RegExp(String.raw`\btn\(\s*[^,]+,\s*${literal}\s*,\s*${second}`, 'g'))) keys.push(unescape(m[2]), unescape(m[4]));
  return keys;
}
/** Static text, placeholders, titles and labels in index.html, as the page translation sees them. */
function pageKeys() {
  const skipped = new Set(['code', 'pre', 'svg', 'script', 'style', 'title']);
  const empty = new Set(['meta', 'link', 'input', 'br', 'hr', 'img', 'use', 'path', 'circle', 'rect']);
  const entity = text => text.replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"');
  const keys = [];
  const open = [];
  let skip = 0;
  for (const token of html.split(/(<[^>]+>)/)) {
    if (!token || token.startsWith('<!')) continue;
    if (token.startsWith('</')) { if (open.pop()) skip--; continue; }
    if (token.startsWith('<')) {
      const name = token.match(/^<([a-zA-Z0-9-]+)/)[1].toLowerCase();
      const untranslated = /translate="no"/.test(token);
      if (!skip && !untranslated) {
        for (const m of token.matchAll(/\b(placeholder|title|aria-label)="([^"]*)"/g)) if (/\p{L}/u.test(m[2])) keys.push(entity(m[2]));
      }
      if (empty.has(name) || token.endsWith('/>')) continue;
      const skips = skipped.has(name) || untranslated;
      open.push(skips);
      if (skips) skip++;
      continue;
    }
    const text = entity(token.replace(/\s+/g, ' ').trim());
    if (!skip && /\p{L}/u.test(text)) keys.push(text);
  }
  return keys;
}
const placeholders = text => [...text.matchAll(/\{(\w+)\}/g)].map(m => m[1]).sort();

test('every interface string has a Spanish translation', () => {
  const spanish = load('es').catalog.es;
  const keys = [...new Set([...scriptKeys(), ...pageKeys()])];
  assert.ok(keys.length > 400, `only ${keys.length} keys found; the extraction broke`);
  assert.deepEqual(keys.filter(key => !(key in spanish)), []);
  for (const key of keys) assert.deepEqual(placeholders(spanish[key]), placeholders(key), key);
});

test('keys are plain text with named placeholders', () => {
  for (const key of scriptKeys()) assert.ok(!key.includes('${'), `interpolated key: ${key}`);
});

test('translation interpolates, counts and formats in the interface language', () => {
  const es = load('es');
  assert.equal(es.t('{ready} of {total} ready', { ready: 2, total: 5 }), '2 de 5 listas');
  assert.equal(es.tn(1, '1 task created', '{n} tasks created'), '1 tarea creada');
  assert.equal(es.tn(3, '1 task created', '{n} tasks created'), '3 tareas creadas');
  assert.equal(es.num(1.5), '1,5');
  assert.equal(es.uiLocale(), 'es-CL');
  const en = load('en', 'en-US');
  assert.equal(en.t('{ready} of {total} ready', { ready: 2, total: 5 }), '2 of 5 ready');
  assert.equal(en.num(1.5), '1.5');
  assert.equal(load('en', 'es-CL').uiLocale(), 'en');
});

test('known host messages are translated and unknown ones stay as they are', () => {
  const es = load('es');
  assert.equal(es.tx('Reading source wiki'), 'Leyendo la fuente wiki');
  assert.equal(es.tx('Planning tasks with the configured agents'), 'Planificando tareas con los agentes configurados');
  assert.equal(es.tx('An activity operation is already running. Wait for it to finish.'), 'Ya hay una operación de actividades en curso. Espera a que termine.');
  assert.equal(es.tx('Could not read source ado: HTTP 503'), 'No se pudo leer la fuente ado: HTTP 503');
  assert.equal(es.tx('Execution machine is offline.'), 'La máquina de ejecución está sin conexión.');
  assert.equal(es.tx('Claude Code is not installed on this desktop.'), 'Claude Code no está instalado en este equipo.');
  assert.equal(es.tx('Set AZURE_IMPLEMENTATION_WRITE_TOKEN in Credentials before creating child tasks.'), 'Configura AZURE_IMPLEMENTATION_WRITE_TOKEN en Credenciales antes de crear tareas hijas.');
  assert.equal(es.tx('Something only the host knows'), 'Something only the host knows');
  assert.equal(load('en').tx('Reading source wiki'), 'Reading source wiki');
});

test('activity names stored in English read in the interface language', () => {
  const es = load('es');
  const cases = {
    'Own message in a chat without a description: ok, le escribí a Felipe': 'Mensaje propio en un chat sin descripción: ok, le escribí a Felipe',
    'Own message in [SopDev] Cobro y Pago Tarifas: revisé el 403\nen prod': 'Mensaje propio en [SopDev] Cobro y Pago Tarifas: revisé el 403\nen prod',
    'Work described in a chat without a description': 'Trabajo descrito en un chat sin descripción',
    'Work described in [SopDev] Cobro y Pago Tarifas': 'Trabajo descrito en [SopDev] Cobro y Pago Tarifas',
    'Call in Daily Cobro Pago y Tarifas': 'Llamada en Daily Cobro Pago y Tarifas',
    "Call ended in Daily Cobro Pago y Tarifas. Its purpose and the user's attendance are not verified.": 'Llamada terminada en Daily Cobro Pago y Tarifas. Su propósito y tu asistencia no están verificados.',
    'Edited Wiki page «Guía de pagos»': 'Página Wiki editada «Guía de pagos»',
    'Commit 1a2b3c4d in portal: fix the receipt': 'Commit 1a2b3c4d en portal: fix the receipt',
    'Pipeline Portal Recaudación, run #29809': 'Pipeline Portal Recaudación, ejecución #29809',
    'PR #12 Fix the receipt (portal)': 'PR #12 Fix the receipt (portal)',
  };
  for (const [english, spanish] of Object.entries(cases)) assert.equal(es.tx(english), spanish);
});
