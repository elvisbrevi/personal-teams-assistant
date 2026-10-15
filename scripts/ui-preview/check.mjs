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
    // Every tab of the section's tab list (Settings sections, Activities views) is scanned.
    const list = { settings: '#settings-tabs', registration: '#activity-tabs' }[tab];
    const groups = list ? await page.$$eval(`${list} [role=tab]`, tabs => tabs.map(t => t.dataset.group)).catch(() => []) : [];
    for (const group of groups.length ? groups : [null]) {
      if (group) { await page.evaluate(([l, g]) => selectTab($(l), g), [list, group]); await page.waitForTimeout(250); }
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
  // Knowledge fold survives a re-render
  await page.click('nav [data-tab="knowledge"]');
  await page.click('#resources .resource:nth-child(3) .resource-edit summary');
  await page.click('#resources .resource:nth-child(3) .resource-toggles input');
  await page.waitForTimeout(200);
  check('source edit fold stays open after toggling', await page.evaluate(() => document.querySelectorAll('#resources .resource')[2].querySelector('.resource-edit').open));
  await page.click('#discard');
  check('no page errors', errors.length === 0, errors.join(' | '));
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
