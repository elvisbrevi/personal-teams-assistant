// axe-core WCAG 2.2 AA scans of every section (and Settings category) plus interaction checks
// of navigation, Settings, conditional fields, Messages search and Knowledge folds.
// Usage: node scripts/ui-preview/check.mjs <port> <out-dir> [--axe-only]   (server.mjs must be serving)
// --axe-only also works against an older copy of the GUI, for a before/after comparison.
import fs from 'node:fs';
import { axePath, launch } from './deps.mjs';

const axeSource = fs.readFileSync(axePath(), 'utf8');
const [port, out, flag] = process.argv.slice(2);
fs.mkdirSync(out, { recursive: true });
const browser = await launch();
const url = scenario => `http://127.0.0.1:${port}/?scenario=${scenario}`;
const results = [];
const check = (name, ok, detail = '') => { results.push({ name, ok, detail }); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`); };

async function open(options = {}, scenario = 'full') {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'es-CL', bypassCSP: !!options.axe, ...options.context });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error' && !m.text().includes('404')) errors.push(m.text()); });
  await page.goto(url(scenario));
  await page.waitForTimeout(600);
  return { context, page, errors };
}

// ---------------------------------------------------------------- axe scans (both versions)
const axeSummary = {};
const scan = page => page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } })).violations
  .map(v => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, sample: v.nodes[0]?.target?.join(' ') })));
for (const [label, ctx] of [['desktop-light', {}], ['desktop-dark', { colorScheme: 'dark' }], ['phone', { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true }]]) {
  const { context, page } = await open({ axe: true, context: ctx });
  await page.evaluate(axeSource);
  for (const tab of ['overview', 'messages', 'registration', 'knowledge', 'chat', 'settings']) {
    await page.evaluate(t => showTab(t), tab);
    await page.waitForTimeout(300);
    // Every tab of the section's tab list (Settings sections, Activities and Knowledge views) is scanned.
    const list = { settings: '#settings-tabs', registration: '#activity-tabs', knowledge: '#knowledge-tabs' }[tab];
    const groups = list ? await page.$$eval(`${list} [role=tab]`, tabs => tabs.map(t => t.dataset.group)).catch(() => []) : [];
    for (const group of groups.length ? groups : [null]) {
      if (group) { await page.evaluate(([l, g]) => selectTab($(l), g), [list, group]); await page.waitForTimeout(250); }
      // Knowledge editors are scanned open.
      if (tab === 'knowledge') await page.evaluate(() => { for (const d of document.querySelectorAll('#knowledge details')) d.open = true; });
      const key = `${label}/${tab}${group ? ':' + group : ''}`;
      axeSummary[key] = await scan(page);
    }
    if (tab === 'messages') {
      // The answer blocks of an unanswered message and of a draft, with their source choosers open.
      await page.evaluate(() => {
        for (const d of document.querySelectorAll('#messages-list details.message')) if (/\/(1004|1008)$/.test(d.dataset.resource)) d.open = true;
        for (const f of document.querySelectorAll('#messages-list .answer-sources')) f.open = true;
      });
      await page.waitForTimeout(250);
      axeSummary[`${label}/messages:answer`] = await scan(page);
    }
  }
  await context.close();
}
fs.writeFileSync(`${out}/axe.json`, JSON.stringify(axeSummary, null, 1));
const totals = {};
for (const [key, list] of Object.entries(axeSummary)) for (const v of list) totals[v.id] = (totals[v.id] || 0) + v.nodes;
console.log('axe violations (rule: nodes across all scans):', JSON.stringify(totals));
if (flag === '--axe-only') { await browser.close(); process.exit(0); }

// ---------------------------------------------------------------- interaction checks
{
  const { context, page, errors } = await open();
  // Navigation order and Settings in the footer
  const order = await page.$$eval('nav [data-tab]', b => b.map(x => x.dataset.tab));
  check('nav order: daily work first, Settings last', JSON.stringify(order) === JSON.stringify(['overview', 'messages', 'registration', 'knowledge', 'chat', 'settings']), order.join(','));
  // Banner only on Home
  const bannerHome = await page.isVisible('#activity-notice');
  await page.click('nav [data-tab="knowledge"]');
  const bannerElsewhere = await page.isVisible('#activity-notice');
  check('activity banner on Home only', bannerHome && !bannerElsewhere);
  check('nav click focuses the section heading', await page.evaluate(() => document.activeElement?.matches('#knowledge h1')));
  // Status row deep link to Settings › Microsoft Teams
  await page.click('nav [data-tab="overview"]');
  await page.click('#tiles .status-row:nth-child(1)');
  await page.waitForTimeout(300);
  check('status row opens Settings › Microsoft Teams', await page.isVisible('#panel-teams') && await page.getAttribute('#settings-tab-teams', 'aria-selected') === 'true');
  // Conditional fields
  check('allowed chats hidden while discovering all chats', !(await page.isVisible('#allowed-chats-field')));
  await page.click('#discover-chats');
  check('allowed chats shown once discovery is off', await page.isVisible('#allowed-chats-field'));
  check('edited section is flagged', await page.isVisible('#settings-tab-teams .flag.edited'));
  check('save bar appears with unsaved changes', await page.isVisible('#savebar'));
  await page.click('#discover-chats');
  // Keyboard tabs
  await page.focus('#settings-tab-teams');
  await page.keyboard.press('ArrowRight');
  check('ArrowRight selects the next settings section', await page.getAttribute('#settings-tab-models', 'aria-selected') === 'true' && await page.evaluate(() => document.activeElement.id) === 'settings-tab-models');
  await page.keyboard.press('End');
  check('End selects the last section', await page.getAttribute('#settings-tab-advanced', 'aria-selected') === 'true');
  // Schedule-dependent fields and weekdays
  await page.click('#settings-tab-activity');
  check('daily schedule shows only its time', await page.isVisible('#registration-at-field') && !(await page.isVisible('#registration-interval-field')));
  await page.selectOption('#registration-schedule', 'interval');
  check('interval schedule shows interval and window', !(await page.isVisible('#registration-at-field')) && await page.isVisible('#registration-interval-field') && await page.isVisible('#registration-end-field'));
  const weekdays = await page.$$eval('#registration-weekdays input', i => i.filter(x => x.checked).map(x => x.dataset.day));
  check('workdays render from the saved list', weekdays.join(',') === '1,2,3,4,5', weekdays.join(','));
  await page.click('#registration-weekdays label:nth-of-type(6)'); // Saturday on
  await page.click('#registration-weekdays label:nth-of-type(1)'); // Monday off
  await page.selectOption('#registration-schedule', 'daily');
  await page.click('#save');
  await page.waitForTimeout(400);
  const saved = await page.evaluate(() => window.__ptaLastSave?.config?.activity_registration);
  check('save sends the chosen workdays as numbers', JSON.stringify(saved?.weekdays) === '[2,3,4,5,6]', JSON.stringify(saved?.weekdays));
  check('save keeps the other registration values', saved?.daily_hours === 9 && saved?.schedule === 'daily' && saved?.at === '18:00' && saved?.interval_minutes === 60);
  check('flags clear after saving', !(await page.isVisible('#settings-tab-activity .flag.edited')) && !(await page.isVisible('#savebar')));
  // No workday: blocked before the host
  for (const box of await page.$$('#registration-weekdays input:checked')) await box.evaluate(b => b.click());
  const before = await page.evaluate(() => window.__ptaLastSave);
  await page.click('#save');
  await page.waitForTimeout(300);
  const after = await page.evaluate(() => window.__ptaLastSave);
  check('saving without workdays is stopped with a message', before === null || JSON.stringify(before) === JSON.stringify(after) ? (await page.textContent('#notice-text')).includes('día hábil') : false, await page.textContent('#notice-text'));
  await page.click('#discard');
  await page.waitForTimeout(300);
  // Messages: search, counts and opening from Home
  await page.click('nav [data-tab="overview"]');
  await page.click('#activity-list .activity-row:nth-child(4)');
  await page.waitForTimeout(500);
  const openRes = await page.evaluate(() => [...document.querySelectorAll('#messages-list details.message[open]')].map(d => d.dataset.resource));
  check('recent activity row opens that message expanded', openRes.includes('chats/19:jkl@thread.v2/messages/1004'), openRes.join(','));
  await page.fill('#messages-search', 'refund');
  await page.waitForTimeout(200);
  const shown = await page.$$eval('#messages-list details.message', d => d.length);
  check('search filters messages', shown === 1, `${shown} shown`);
  await page.fill('#messages-search', 'revisión inexistente');
  check('empty search offers to clear it', await page.isVisible('#messages-list .empty button'));
  await page.click('#messages-list .empty button');
  const counts = await page.$$eval('#messages-filters .count', c => c.map(x => x.textContent));
  check('filters show counts', counts[0] === '7', counts.join('|'));
  // An unanswered message: generate an answer from chosen sources, then regenerate or send it.
  const informational = '#messages-list details[data-resource="chats/19:stu@thread.v2/messages/1008"]';
  await page.click(`${informational} summary`);
  check('unanswered message offers to generate an answer, without context or send yet',
    await page.isVisible(`${informational} .draft-actions button:text("Generar respuesta")`)
    && !(await page.isVisible(`${informational} .answer-context`)) && !(await page.isVisible(`${informational} button:text("Enviar respuesta")`)));
  check('source chooser starts automatic and closed', (await page.textContent(`${informational} .answer-sources summary`)).includes('Automáticas')
    && !(await page.$eval(`${informational} .answer-sources`, d => d.open)));
  await page.click(`${informational} .answer-sources summary`);
  check('this chat\'s authorized sources are marked', /autorizada para este chat/.test(await page.textContent(`${informational} .answer-sources`)));
  await page.click(`${informational} .answer-sources input[value="wiki"]`);
  check('choosing a source updates the summary', (await page.textContent(`${informational} .answer-sources summary`)).includes('1 elegida'));
  await page.click(`${informational} .draft-actions button:text("Generar respuesta")`);
  await page.waitForTimeout(300);
  const refined = await page.evaluate(() => window.__ptaLastRefine);
  check('generate sends the chosen sources and no context', JSON.stringify(refined?.sources) === '["wiki"]' && refined?.context === '', JSON.stringify(refined));
  await page.waitForTimeout(3600);
  check('once written: context, regenerate and send',
    await page.isVisible(`${informational} .answer-context`)
    && await page.isVisible(`${informational} button:text("Volver a generar respuesta")`)
    && await page.isVisible(`${informational} button:text("Enviar respuesta")`));
  check('the written answer is shown', /Development Bug/.test(await page.textContent(informational)));
  check('the last chosen source stays chosen', await page.$eval(`${informational} .answer-sources input[value="wiki"]`, b => b.checked));
  await page.screenshot({ path: `${out}/desktop-messages-answer.png`, fullPage: true });
  const send = page.locator(`${informational} button:text("Enviar respuesta")`);
  await send.click();
  check('sending asks for a second tap', await page.isVisible(`${informational} button:text("Toca de nuevo para enviar")`));
  await page.click(`${informational} button:text("Toca de nuevo para enviar")`);
  await page.waitForTimeout(400);
  check('a sent answer offers nothing more', !(await page.isVisible(`${informational} .draft-actions`)));
  // Rating an answer: good, as an example, with a comment; then saving it as a note.
  const rated = '#messages-list details.message[data-resource$="/1001"]';
  await page.evaluate(sel => { document.querySelector(sel).open = true; }, rated);
  await page.click(`${rated} .feedback button:text("Buena respuesta")`);
  check('rating good offers to keep it as an example', await page.isVisible(`${rated} .example-fields`));
  await page.click(`${rated} .example-fields input[type=checkbox]`);
  await page.fill(`${rated} .feedback-comment`, 'Exactly the steps I use.');
  await page.click(`${rated} .feedback button:text("Guardar evaluación")`);
  await page.waitForTimeout(300);
  check('the rating is saved with the example', await page.evaluate(() => {
    const f = window.__ptaLastFeedback;
    return f?.rating === 'good' && f.example === true && f.comment === 'Exactly the steps I use.';
  }));
  await page.evaluate(() => { document.querySelector('#messages-list details.message[data-resource$="/1002"]').open = true; });
  check('a known sender links to their profile', await page.isVisible('#messages-list details.message[data-resource$="/1002"] .link-button:text("Cómo te diriges a Ana Pérez")'));
  await page.evaluate(sel => { document.querySelector(sel).open = true; }, rated);
  await page.click(`${rated} .link-button:text("Agregar a Luis Soto en Personas")`);
  check('an unknown sender opens People with their name and Teams ID', await page.inputValue('#person-name') === 'Luis Soto'
    && (await page.inputValue('#person-sender')).endsWith('b2'));
  await page.click('#add-person');
  check('the person is added', await page.isVisible('#people .resource:has-text("Luis Soto")'));
  await page.click('#discard');
  await page.click('nav [data-tab="messages"]');
  await page.evaluate(sel => { document.querySelector(sel).open = true; }, rated);
  await page.click(`${rated} .feedback button:text("Guardar como nota")`);
  check('saving an answer as a note opens the note form with it', await page.isVisible('#note-form[open]')
    && (await page.inputValue('#note-text')).includes('rollback runbook'));
  // Knowledge fold survives a re-render
  await page.click('nav [data-tab="knowledge"]');
  await page.click('#knowledge-tab-sources');
  await page.click('#resources .resource:nth-child(3) .resource-edit summary');
  await page.click('#resources .resource:nth-child(3) .resource-toggles input');
  await page.waitForTimeout(200);
  check('source edit fold stays open after toggling', await page.evaluate(() => document.querySelectorAll('#resources .resource')[2].querySelector('.resource-edit').open));
  check('the activity record is offered while the map has none', await page.isVisible('#record-offer'));
  await page.click('#add-record');
  await page.waitForTimeout(200);
  check('adding the activity record lists it disabled and hides the offer', !(await page.isVisible('#record-offer'))
    && await page.evaluate(() => [...document.querySelectorAll('#resources .resource')].some(r => r.textContent.includes('activity-record')
      && r.textContent.includes('Registro de actividades') && r.textContent.includes('Desactivado'))));
  check('a repository source shows what its index holds', await page.evaluate(() => [...document.querySelectorAll('#resources .resource')]
    .some(r => r.textContent.includes('handbook') && r.textContent.includes('42 archivos'))));
  await page.click('#knowledge-sources details.add-form summary');
  await page.selectOption('#source-kind', 'repository');
  check('a whole repository needs no file path', !(await page.isVisible('#source-path-label')));
  await page.selectOption('#source-kind', 'file');
  check('one file asks for its path', await page.isVisible('#source-path-label'));
  await page.click('#discard');
  // Contexts: a subject with its keywords, sources and instructions, saved with the map.
  await page.click('#knowledge-tab-contexts');
  check('contexts list shows the billing context', await page.isVisible('#contexts .resource:has-text("Billing")'));
  await page.click('#contexts .resource-edit summary');
  await page.click('#contexts .resource-edit input[value="handbook"]');
  check('choosing a context source asks to save', await page.isVisible('#savebar'));
  await page.click('#save');
  await page.waitForTimeout(300);
  check('the context is saved with its new source', await page.evaluate(() => window.__ptaLastSave?.map?.contexts?.[0]?.sources?.includes('handbook')));
  // Cases: the built-in kinds of request, edited and restored; only changes are saved.
  await page.click('#knowledge-tab-cases');
  check('the eight built-in cases are listed', await page.$$eval('#cases .resource', rows => rows.length) === 8);
  const status = '#cases .resource:has-text("Ronda de avance")';
  await page.click(`${status} .resource-edit summary`);
  check('a case shows its default instructions to edit', await page.inputValue(`${status} textarea`) === 'Default instructions of status_round: how it is answered, part by part.');
  await page.fill(`${status} input:not([type=checkbox])`, 'entrega, despliegue');
  await page.click(`${status} input[value="ado-activity"]`);
  check('a changed case is marked', await page.isVisible(`${status} .badge:has-text("Cambiado")`));
  await page.click('#save');
  await page.waitForTimeout(300);
  check('only the change is saved', await page.evaluate(() => JSON.stringify(window.__ptaLastSave?.map?.cases)
    === JSON.stringify([{ id: 'status_round', keywords: ['entrega', 'despliegue'], sources: ['ado-activity'] }])));
  await page.click('#knowledge-tab-cases');
  check('an open case stays open after saving', await page.isVisible(`${status} button:has-text("Restaurar")`));
  await page.click(`${status} button:has-text("Restaurar")`);
  await page.click('#save');
  await page.waitForTimeout(300);
  check('restoring defaults removes the change', await page.evaluate(() => (window.__ptaLastSave?.map?.cases ?? []).length === 0));
  // Notes: searched, added right away with their contexts.
  await page.click('#knowledge-tab-notes');
  await page.waitForTimeout(200);
  check('notes are listed', await page.$$eval('#notes .resource', rows => rows.length) === 2);
  check('the notes source is offered while none reads them', await page.isVisible('#notes-offer'));
  await page.fill('#notes-query', 'hotfix');
  await page.waitForTimeout(500);
  check('searching notes narrows the list', await page.$$eval('#notes .resource', rows => rows.length) === 1);
  await page.fill('#notes-query', '');
  await page.evaluate(() => { document.querySelector('#note-form').open = false; });
  await page.click('#note-form summary');
  await page.fill('#note-title', 'Refund cutoff');
  await page.fill('#note-text', 'Refunds requested after 18:00 are processed the next business day.');
  await page.click('#note-contexts input[value="billing"]');
  await page.click('#add-note');
  await page.waitForTimeout(400);
  check('a new note is saved with its context', await page.evaluate(() => window.__ptaLastNote?.title === 'Refund cutoff' && window.__ptaLastNote?.contexts?.[0] === 'billing'));
  check('the new note is listed', await page.isVisible('#notes .resource:has-text("Refund cutoff")'));
  await page.click('#knowledge-tab-sources');
  // Semantic search: endpoint and model together; the credential is mapped in [secrets].
  await page.evaluate(() => showTab('settings', 'panel-embeddings'));
  await page.fill('#embeddings-url', 'https://api.openai.com/v1');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('an endpoint without a model is refused before saving', await page.isVisible('#notice.error'));
  await page.fill('#embeddings-model', 'text-embedding-3-small');
  await page.fill('#embeddings-credential', 'OPENAI_API_KEY');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('embeddings are saved with their credential reference', await page.evaluate(() => {
    const config = window.__ptaLastSave?.config;
    return config?.knowledge_index?.embeddings?.secret_ref === 'secret://knowledge/embeddings' && config.secrets['secret://knowledge/embeddings'] === 'OPENAI_API_KEY';
  }));
  check('no page errors', errors.length === 0, errors.join(' | '));
  await context.close();
}
{
  // Theme: this device's choice applies at once, asks for no save and survives a reload.
  const { context, page } = await open();
  const theme = () => page.evaluate(() => [document.documentElement.dataset.theme ?? null, getComputedStyle(document.body).backgroundColor, localStorage.getItem('pta-theme')]);
  await page.click('nav [data-tab="settings"]');
  check('theme starts matching the system', JSON.stringify(await theme()) === JSON.stringify([null, 'rgb(255, 246, 229)', null])
    && await page.getAttribute('#theme-choice [data-theme-choice="system"]', 'aria-pressed') === 'true', JSON.stringify(await theme()));
  await page.click('#theme-choice [data-theme-choice="dark"]');
  check('dark theme applies right away without asking to save', JSON.stringify(await theme()) === JSON.stringify(['dark', 'rgb(23, 21, 31)', 'dark'])
    && !(await page.isVisible('#savebar')), JSON.stringify(await theme()));
  await page.reload();
  await page.waitForTimeout(600);
  check('the chosen theme survives a reload', (await theme())[0] === 'dark' && await page.getAttribute('#theme-choice [data-theme-choice="dark"]', 'aria-pressed') === 'true');
  const bars = await page.$$eval('meta[name="theme-color"]', metas => metas.map(m => m.content));
  check('the browser bar takes the chosen theme', bars.length === 2 && bars.every(color => color === 'rgb(23, 21, 31)'), bars.join(' | '));
  await page.evaluate(() => showTab('settings'));
  await page.click('#theme-choice [data-theme-choice="system"]');
  check('matching the system again forgets the choice', JSON.stringify(await theme()) === JSON.stringify([null, 'rgb(255, 246, 229)', null]), JSON.stringify(await theme()));
  // The tab icon, the iOS home-screen icon and the manifest with every icon it names load under the portal's CSP.
  const icons = await page.evaluate(async () => {
    const manifest = await (await fetch(document.querySelector('link[rel=manifest]').href)).json();
    const urls = [document.querySelector('link[rel=icon]').href, document.querySelector('link[rel=apple-touch-icon]').href,
      ...manifest.icons.map(icon => new URL(icon.src, location.href).href)];
    const loaded = await Promise.all(urls.map(async url => (await fetch(url)).ok));
    return { display: manifest.display, loaded };
  });
  check('the tab icon, the home-screen icon and the manifest\'s icons load', icons.display === 'standalone' && icons.loaded.length === 4 && icons.loaded.every(Boolean), JSON.stringify(icons));
  await context.close();
}
{
  const { context, page } = await open({}, 'fresh');
  check('fresh: setup checklist visible', await page.isVisible('#setup-panel'));
  await page.click('#setup-steps li:first-child button');
  await page.waitForTimeout(300);
  check('fresh: setup step opens Settings › Credentials', await page.getAttribute('#settings-tab-credentials', 'aria-selected') === 'true' && await page.isVisible('#panel-credentials'));
  check('fresh: credentials and Teams flagged', await page.isVisible('#settings-tab-credentials .flag.warn') && await page.isVisible('#settings-tab-teams .flag.warn'));
  await context.close();
}
{
  const { context, page } = await open({ context: { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true } });
  const columns = await page.$eval('nav', n => getComputedStyle(n).gridTemplateColumns.split(' ').length);
  check('phone: five sections in the tab bar', columns === 5, `${columns} columns`);
  const box = await page.locator('nav .nav-settings').boundingBox();
  check('phone: Settings sits in the title bar', box && box.y < 60 && box.width >= 44, JSON.stringify(box));
  await page.click('nav .nav-settings');
  check('phone: Settings opens', await page.isVisible('#settings-tabs'));
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  check('phone: no horizontal page scroll', !overflow);
  const tabsScroll = await page.$eval('#settings-tabs', n => n.scrollWidth > n.clientWidth);
  check('phone: Settings sections all in sight, none scrolls sideways', !tabsScroll);
  await page.click('nav [data-tab="registration"]');
  await page.tap('#activity-tab-log');
  await page.waitForTimeout(400);
  const days = await page.$$eval('#activity-log .log-day h3', h => h.map(x => x.textContent));
  check('phone: History lists the registered work by day, newest first', days.length === 4, days.join(' | '));
  check('phone: History counts the approved tasks', /5 tareas/.test(await page.textContent('#activity-log-total')), await page.textContent('#activity-log-total'));
  await page.click('nav [data-tab="messages"]');
  await page.waitForTimeout(400);
  await page.tap('#messages-list details[data-resource$="/1008"] summary');
  await page.tap('#messages-list details[data-resource$="/1008"] .answer-sources summary');
  await page.waitForTimeout(200);
  const generateBox = await page.locator('#messages-list details[data-resource$="/1008"] .draft-actions button').boundingBox();
  const checkBox = await page.locator('#messages-list details[data-resource$="/1008"] .answer-sources label').first().boundingBox();
  check('phone: generate answer and its source choices are 44px targets', generateBox?.height >= 44 && checkBox?.height >= 44, JSON.stringify([generateBox, checkBox]));
  check('phone: an open answer block does not scroll sideways', !(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)));
  await page.screenshot({ path: `${out}/phone-messages-answer.png`, fullPage: true });
  await page.click('nav [data-tab="registration"]');
  await page.tap('#activity-tab-review');
  check('phone: Review comes back', await page.isVisible('#activity-day-panel') && !(await page.isVisible('#activity-view-log')));
  for (const g of ['general', 'teams', 'models', 'activity', 'credentials', 'advanced']) {
    await page.evaluate(x => selectTab($('#settings-tabs'), x), g);
    await page.waitForTimeout(150);
    await page.screenshot({ path: `${out}/phone-settings-${g}.png`, fullPage: true });
  }
  await context.close();
}
{
  const { context, page } = await open();
  await page.click('nav [data-tab="settings"]');
  for (const g of ['general', 'teams', 'models', 'activity', 'credentials', 'advanced']) {
    await page.click(`#settings-tab-${g}`);
    await page.waitForTimeout(150);
    await page.screenshot({ path: `${out}/desktop-settings-${g}.png`, fullPage: true });
  }
  await context.close();
}
await browser.close();
fs.writeFileSync(`${out}/flows.json`, JSON.stringify(results, null, 1));
const failed = results.filter(r => !r.ok).length;
console.log(`${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
