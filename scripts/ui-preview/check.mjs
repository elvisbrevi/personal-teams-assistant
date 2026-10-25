// axe-core WCAG 2.2 AA scans of every section (and Settings category) plus interaction checks
// of navigation, Settings, conditional fields, Messages search and Knowledge folds.
// Usage: node scripts/ui-preview/check.mjs <port> <out-dir> [--axe-only]   (server.mjs must be serving)
// It exits 1 when an interaction check fails or any scan finds a violation (CI runs it).
// --axe-only only reports, and also works against an older copy of the GUI, for a before/after comparison.
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
  // Scans see the settled page: with reduced motion the GUI has no transitions, so a tab is never
  // measured halfway between its colors (on a slow machine that read as a contrast failure).
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'es-CL', bypassCSP: !!options.axe,
    ...(options.axe ? { reducedMotion: 'reduce' } : {}), ...options.context });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error' && !m.text().includes('404')) errors.push(m.text()); });
  await page.goto(url(scenario));
  await page.waitForTimeout(600);
  return { context, page, errors };
}

// Native date/time controls must fit their label, including an empty end date.
{
  const { context, page } = await open({ context: { viewport: { width: 320, height: 780 }, isMobile: true, hasTouch: true } });
  const fits = () => page.locator('input[type=date]:visible, input[type=time]:visible').evaluateAll(inputs => inputs.length > 0 && inputs.every(input => {
    const field = input.getBoundingClientRect(), parent = input.parentElement.getBoundingClientRect();
    return field.left >= parent.left - 1 && field.right <= parent.right + 1 && field.width <= parent.width + 1;
  }));
  await page.click('nav [data-tab="registration"]');
  await page.waitForTimeout(200);
  check('phone date filters fit their fields', await fits());
  await page.click('nav [data-tab="settings"]');
  await page.click('#settings-tab-activity');
  check('phone daily time fits its field', await fits());
  await page.selectOption('#registration-schedule', 'interval');
  check('phone interval times fit their fields', await fits());
  await context.close();
}

// ---------------------------------------------------------------- axe scans (both versions)
const axeSummary = {};
const scan = page => page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } })).violations
  .map(v => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, sample: v.nodes[0]?.target?.join(' ') })));
for (const [label, ctx] of [['desktop-light', {}], ['desktop-dark', { colorScheme: 'dark' }], ['phone', { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true }]]) {
  const { context, page } = await open({ axe: true, context: ctx });
  await page.evaluate(axeSource);
  for (const tab of ['overview', 'todo', 'agent-history', 'messages', 'registration', 'knowledge', 'chat', 'settings']) {
    await page.evaluate(t => showTab(t), tab);
    await page.waitForTimeout(300);
    // Every tab of the section's tab list (Settings sections, Activities and Knowledge views) is scanned.
    const list = { settings: '#settings-tabs', knowledge: '#knowledge-tabs' }[tab];
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
  await page.click('nav [data-tab="todo"]');
  await page.waitForTimeout(200);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  await page.evaluate(() => { for (const fold of document.querySelectorAll('#todo-detail-content details')) fold.open = true; });
  axeSummary[label + '-todo-detail'] = await scan(page);
  check(`${label} To-Do detail WCAG 2.2 AA`, axeSummary[label + '-todo-detail'].length === 0, JSON.stringify(axeSummary[label + '-todo-detail']));
  await page.click('#todo-back');
  await page.selectOption('#todo-kind', 'bug');
  await page.waitForTimeout(200);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  axeSummary[label + '-bug-detail'] = await scan(page);
  await page.click('#todo-back');
  await page.selectOption('#todo-provider', 'github');
  await page.waitForTimeout(200);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  axeSummary[label + '-epic-detail'] = await scan(page);
  await context.close();
}
fs.writeFileSync(`${out}/axe.json`, JSON.stringify(axeSummary, null, 1));
const totals = {};
for (const [key, list] of Object.entries(axeSummary)) for (const v of list) totals[v.id] = (totals[v.id] || 0) + v.nodes;
console.log('axe violations (rule: nodes across all scans):', JSON.stringify(totals));
if (flag === '--axe-only') { await browser.close(); process.exit(0); }
check('axe: no WCAG 2.2 AA violations in any scan', !Object.keys(totals).length, `${Object.keys(axeSummary).length} scans`);

// ---------------------------------------------------------------- interaction checks
{
  const { context, page, errors } = await open();
  // Navigation order and Settings in the footer
  const order = await page.$$eval('nav [data-tab]', b => b.map(x => x.dataset.tab));
  check('nav order: daily work first, Settings last', JSON.stringify(order) === JSON.stringify(['overview', 'todo', 'messages', 'registration', 'agent-history', 'knowledge', 'chat', 'settings']), order.join(','));
  await page.click('nav [data-tab="todo"]');
  await page.waitForTimeout(200);
  check('To-Do initially requests ten Azure HUs', await page.evaluate(() => window.__ptaLastTodo?.provider === 'azure' && window.__ptaLastTodo?.limit === 10) && await page.locator('.todo-item').count() === 10);
  check('To-Do includes states without assigned items', await page.locator('#todo-state option[value="Awaiting validation"]').count() === 1);
  check('To-Do keeps both HU and development date pairs', await page.locator('.todo-item:first-child .todo-dates dt').count() === 5);
  await page.click('.todo-item:first-child .todo-actions .icon-button:nth-child(2)');
  await page.waitForFunction(() => window.__ptaOpenedWorkItem);
  check('native To-Do link uses the host browser operation', await page.evaluate(() => window.__ptaOpenedWorkItem?.endsWith('/5000')));
  await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async url => { window.__ptaCopiedWorkItemUrl = url; } } }));
  await page.click('.todo-item:first-child .todo-actions .icon-button:nth-child(3)');
  await page.waitForFunction(() => window.__ptaCopiedWorkItemUrl);
  check('To-Do copy action copies the work item URL and confirms it', await page.evaluate(() => window.__ptaCopiedWorkItemUrl?.endsWith('/5000')) && await page.getByRole('status').filter({ hasText: 'Enlace copiado:' }).count() === 1);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  check('To-Do opens HU detail through the shared tool with source and project', await page.evaluate(() => window.__ptaLastDetail?.id === 5000 && window.__ptaLastDetail?.source === 'ado-activity' && window.__ptaLastDetail?.project === 'Payments'));
  check('HU detail includes assignment, acceptance criteria, evidence, discussion and original link', (await page.textContent('#todo-detail-content')).includes('Ana Pérez') && await page.getByRole('heading', { name: 'Criterios de aceptación' }).count() === 1 && (await page.textContent('#todo-detail-content')).includes('Luis Soto') && await page.locator('#todo-detail-content .todo-detail-header .icon-button:visible').count() === 3 && await page.locator('#todo-detail-content .todo-detail-header button[aria-label="Copiar enlace del work item"]').count() === 1);
  await page.locator('#todo-detail-content details').first().locator('summary').click();
  check('HU detail retains custom fields', (await page.textContent('#todo-detail-content details')).includes('feature/receipt-downloads'));
  const safeRichText = await page.evaluate(() => {
    const probe = todoRich('<script>window.__ptaUnsafe = true</script><p style="color:red" onclick="window.__ptaUnsafe = true">Safe <strong>text</strong></p><img src="https://untrusted.invalid/image.png" onerror="window.__ptaUnsafe = true"><a href="javascript:alert(1)">Unsafe link</a><iframe src="https://untrusted.invalid/"></iframe>', 'html', 'https://dev.azure.com/contoso/Payments/_workitems/edit/5000');
    document.body.append(probe);
    const safe = !probe.querySelector('img, script, iframe, [style], [onclick], [onerror], [href^="javascript:"]') && !window.__ptaUnsafe && probe.textContent.includes('Safe');
    probe.remove();
    return safe;
  });
  check('Provider rich text keeps formatting while removing executable content and remote images', safeRichText);
  check('Plain Markdown is formatted, code stays literal and credential URLs stay inactive', await page.evaluate(() => {
    const base = 'https://dev.azure.com/contoso/Payments/_workitems/edit/5000';
    const plain = todoRich('# Evidence\n\n- Passed\n\nhttps://dev.azure.com/contoso/Payments/_build/results?buildId=42', 'plainText', base);
    const code = todoRich('', 'html', base, [{ tag: 'pre', children: [{ tag: 'code', children: [{ tag: 'text', text: '# Literal code\nhttps://example.test/' }] }] }]);
    const unsafe = todoRich('https://secret@example.test/', 'markdown', base);
    return !!plain.querySelector('h2') && !!plain.querySelector('li') && !!plain.querySelector('.ref') && !!code.querySelector('pre code') && !code.querySelector('.ref, h2') && !unsafe.querySelector('.ref');
  }));
  await page.click('#todo-back');
  check('Back restores the original list and keyboard focus', await page.isVisible('#todo-master') && await page.evaluate(() => document.activeElement?.classList.contains('todo-select')));
  for (let index = 1; index < 10; index++) {
    await page.locator('#todo-list .todo-select').nth(index).click();
    await page.waitForSelector('#todo-detail-title');
    check(`To-Do card ${index + 1} opens its own detail`, await page.evaluate(id => window.__ptaLastDetail?.id === id, 5000 - index));
    await page.click('#todo-back');
  }
  await page.click('#todo-list .todo-select');
  await page.waitForSelector('#todo-detail-title');
  check('Embedded Markdown evidence renders headings, lists and links', await page.getByRole('heading', { name: 'Development evidence', exact: true }).count() === 1 && await page.locator('#todo-detail-content .md li:visible').filter({ hasText: 'Passed 18 tests' }).count() === 1 && await page.getByRole('button', { name: 'Pull request', exact: true }).count() > 0);
  check('Bare comment URLs become links without trailing punctuation', (await page.locator('.todo-comment .ref').getAttribute('title')).endsWith('https://dev.azure.com/contoso/Payments/_release?releaseId=42&_a=release-summary'));
  const relationFilter = page.locator('#todo-detail-content .todo-relation-filter select');
  await relationFilter.selectOption('Task');
  check('Relations filter selects work item type and reuses task cards', await page.locator('#todo-detail-content .todo-list .todo-item').count() === 2 && await page.locator('#todo-detail-content .todo-relation').count() === 0);
  await page.locator('#todo-detail-content .todo-list .todo-select').nth(1).click();
  await page.waitForFunction(() => window.__ptaLastDetail?.id === 4503 && window.__ptaLastDetail?.method === 'get_azure_task');
  check('A child task opens the shared task detail inside To-Do', (await page.textContent('#todo-detail-title')).includes('#4503') && await page.isVisible('#todo-detail'));
  await page.click('#todo-back');
  check('Back from a child restores the HU, relation filter and focus', (await page.textContent('#todo-detail-title')).includes('#5000') && await relationFilter.inputValue() === 'Task' && await page.evaluate(() => document.activeElement?.textContent.includes('#4503')));
  await page.evaluate(() => { window.__ptaDetailDelay = 250; });
  await page.locator('#todo-detail-content .todo-list .todo-select').first().click();
  await page.click('#todo-back');
  await page.waitForTimeout(350);
  check('Back during a child read discards its late response', (await page.textContent('#todo-detail-title')).includes('#5000') && await relationFilter.inputValue() === 'Task');
  await page.evaluate(() => { window.__ptaDetailDelay = 0; });
  await relationFilter.selectOption('Bug');
  check('Other work item types remain filterable and retain their original links', await page.locator('#todo-detail-content .todo-list .todo-item').count() === 0 && await page.locator('#todo-detail-content .todo-relation .ref').count() === 1);
  await relationFilter.selectOption('__links');
  check('Evidence and attachments remain accessible through Other links', await page.locator('#todo-detail-content .todo-relation').count() === 2 && await page.locator('#todo-detail-content .todo-list .todo-item').count() === 0);
  await page.click('#todo-back');
  await page.selectOption('#todo-kind', 'bug');
  await page.waitForTimeout(200);
  check('Azure bug filter reaches the tool and keeps only bugs', await page.evaluate(() => window.__ptaLastTodo?.kind === 'bug') && await page.locator('.todo-item').count() === 5);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  check('Bug detail uses reproduction steps and severity', await page.getByRole('heading', { name: 'Pasos de reproducción' }).count() === 1 && (await page.textContent('#todo-detail-content')).includes('2 - High'));
  await page.evaluate(() => { window.__ptaDetailError = true; });
  await page.click('#todo-detail-refresh');
  await page.waitForTimeout(200);
  check('Detail failures are inline and retain the original link and copy action', await page.isVisible('#todo-detail-problem') && await page.locator('#todo-detail-content .icon-button:visible').count() === 3 && await page.locator('#todo-detail-content .todo-actions button[aria-label="Copiar enlace del work item"]').count() === 1);
  await page.evaluate(() => { window.__ptaDetailError = false; });
  await page.click('#todo-back');
  check('Detail back keeps the type filter', await page.inputValue('#todo-kind') === 'bug');
  await page.selectOption('#todo-kind', '');
  await page.selectOption('#todo-state', 'Finished');
  await page.waitForTimeout(200);
  check('To-Do state filter reaches the backend tool', await page.evaluate(() => window.__ptaLastTodo?.state === 'Finished') && await page.locator('.todo-item').count() === 7);
  await page.selectOption('#todo-state', 'Awaiting validation');
  await page.waitForTimeout(200);
  check('To-Do has a filtered empty state', await page.isVisible('#todo-list .empty'));
  await page.selectOption('#todo-state', '');
  await page.selectOption('#todo-limit', '25');
  await page.waitForTimeout(200);
  check('To-Do limit reaches the backend tool', await page.evaluate(() => window.__ptaLastTodo?.limit === 25) && await page.locator('.todo-item').count() === 15);
  await page.evaluate(() => { window.__ptaTodoError = true; });
  await page.click('#todo-refresh');
  await page.waitForTimeout(200);
  check('To-Do reports read errors inline and clears old items', await page.isVisible('#todo-problem') && await page.locator('.todo-item').count() === 0);
  await page.evaluate(() => { window.__ptaTodoError = false; window.__ptaTodoDelay = 250; });
  await page.click('#todo-refresh');
  await page.selectOption('#todo-provider', 'github');
  await page.waitForTimeout(400);
  check('To-Do ignores an older Azure response after switching provider', await page.locator('.todo-item').count() === 1 && (await page.textContent('#todo-list')).includes('Billing platform'));
  check('GitHub offers its native issue states and unavailable dates', await page.locator('#todo-state option[value="closed"]').count() === 1 && (await page.textContent('#todo-list')).includes('Sin registrar'));
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  check('GitHub epic detail passes repository-local ID and includes sub-issues', await page.evaluate(() => window.__ptaLastDetail?.provider === 'github' && window.__ptaLastDetail?.id === 42 && window.__ptaLastDetail?.project === 'contoso/billing') && (await page.textContent('#todo-detail-content')).includes('#43'));
  await page.click('#todo-back');
  await page.evaluate(() => { window.__ptaDetailDelay = 250; });
  await page.click('.todo-item:first-child .todo-select');
  await page.click('#todo-back');
  await page.waitForTimeout(350);
  check('Leaving detail discards a late response', await page.isVisible('#todo-master') && !await page.isVisible('#todo-detail'));
  await page.evaluate(() => { window.__ptaDetailDelay = 0; });
  await page.evaluate(() => { window.ptaTransport.browser = true; });
  await page.click('#todo-refresh');
  await page.waitForTimeout(200);
  check('portal To-Do links open a new tab with opener protection', await page.locator('#todo-list a[target="_blank"][rel="noopener noreferrer"]').count() === 1);
  await page.evaluate(() => { window.ptaTransport.browser = false; });
  await page.evaluate(() => { window.__ptaTodoDelay = 0; });
  await page.screenshot({ path: `${out}/desktop-todo.png`, fullPage: true });
  await page.click('nav [data-tab="overview"]');
  // The daily record no longer leaves obsolete input warnings on Home.
  const bannerHome = await page.isVisible('#activity-notice');
  await page.click('nav [data-tab="knowledge"]');
  const bannerElsewhere = await page.isVisible('#activity-notice');
  check('no obsolete activity input banner', !bannerHome && !bannerElsewhere);
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
  // Repeated providers survive model selection, reordering, removal and saving.
  await page.click('#settings-tab-models');
  const modelCount = await page.locator('#llm-chain > li').count();
  await page.selectOption('#add-llm-provider', 'codex');
  await page.click('#add-llm');
  check('adding another model keeps the same provider', await page.locator('#llm-chain > li').count() === modelCount + 1);
  await page.locator('#llm-chain > li:last-child .field select').first().selectOption('gpt-6-astra');
  await page.locator('#llm-chain > li:last-child .order button').first().click();
  await page.click('#save');
  await page.waitForTimeout(400);
  const savedChain = await page.evaluate(() => window.__ptaLastSave.config.llm.chain);
  check('saving retains repeated providers with independent models and priority', savedChain.filter(c => c.provider === 'codex').length === 2 && savedChain[2].model === 'gpt-6-astra' && savedChain[0].model !== savedChain[2].model);
  await page.locator('#llm-chain > li:nth-child(3) .order button').last().click();
  await page.selectOption('#add-llm-provider', 'opencode');
  await page.click('#add-llm');
  const openCodeRow = page.locator('#llm-chain > li:last-child');
  check('OpenCode model is a dropdown without free text', await openCodeRow.locator('.field input, datalist').count() === 0 && await openCodeRow.locator('.field select').count() === 2);
  check('OpenCode lists only catalog favorites', JSON.stringify(await openCodeRow.locator('.field select').first().locator('option').evaluateAll(xs => xs.map(x => x.value))) === JSON.stringify(['example/model-a', 'example/model-b']));
  check('OpenCode efforts match model A variants', JSON.stringify(await openCodeRow.locator('.field select').last().locator('option').evaluateAll(xs => xs.map(x => x.value))) === JSON.stringify(['default', 'none', 'high']));
  await openCodeRow.locator('.field select').last().selectOption('high');
  await openCodeRow.locator('.field select').first().selectOption('example/model-b');
  check('switching OpenCode models resets an unsupported effort to default', await openCodeRow.locator('.field select').last().inputValue() === 'default');
  check('OpenCode efforts match model B including named variants', JSON.stringify(await openCodeRow.locator('.field select').last().locator('option').evaluateAll(xs => xs.map(x => x.value))) === JSON.stringify(['default', 'minimal', 'deep']));
  await openCodeRow.locator('.field select').last().selectOption('deep');
  await page.click('#save');
  await page.waitForTimeout(400);
  check('OpenCode saves its selected favorite and native variant', await page.evaluate(() => window.__ptaLastSave.config.llm.chain.at(-1).provider === 'opencode' && window.__ptaLastSave.config.llm.chain.at(-1).model === 'example/model-b' && window.__ptaLastSave.config.llm.chain.at(-1).effort === 'deep'));
  await page.locator('#llm-chain > li:last-child .order button').last().click();
  await page.click('#save');
  await page.waitForTimeout(400);
  check('removing a model persists without dropping other entries', await page.evaluate(() => window.__ptaLastSave.config.llm.chain.length) === modelCount);
  await page.evaluate(() => {
    llmRows.push({ provider: 'opencode', model: 'example/no-longer-favorite', effort: 'high', enabled: false });
    markDirty({ target: document.querySelector('#llm-chain') });
    renderChain();
  });
  check('a saved non-favorite is preserved without becoming a model option', await openCodeRow.locator('.field select').first().inputValue() === '' && await openCodeRow.locator('option[value="example/no-longer-favorite"]').count() === 0 && (await openCodeRow.textContent()).includes('example/no-longer-favorite'));
  await page.click('#save');
  await page.waitForTimeout(400);
  check('unrelated settings saves preserve the old non-favorite model', await page.evaluate(() => window.__ptaLastSave.config.llm.chain.at(-1).model === 'example/no-longer-favorite'));
  await page.evaluate(() => {
    llmRows.at(-1).model = 'example/model-a';
    llmRows.at(-1).effort = 'deep';
    renderChain();
  });
  check('a saved unsupported effort is not offered as a variant', await openCodeRow.locator('.field select').last().inputValue() === '' && await openCodeRow.locator('.field select').last().locator('option[value="deep"]').count() === 0);
  await page.evaluate(() => {
    llmCatalog.providers.find(p => p.id === 'opencode').models = [];
    renderChain();
  });
  check('no OpenCode favorites disables Add and model selection', await page.isDisabled('#add-llm') && await openCodeRow.locator('.field select').first().isDisabled() && await openCodeRow.locator('.field select').last().isDisabled());
  await page.locator('#llm-chain > li:last-child .order button').last().click();
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
  check('filters show counts', counts[0] === '8', counts.join('|'));
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
  check('the default question already has its activity-record source', !(await page.isVisible('#record-offer')));
  check('a repository source shows what its index holds', await page.evaluate(() => [...document.querySelectorAll('#resources .resource')]
    .some(r => r.textContent.includes('handbook') && r.textContent.includes('42 archivos'))));
  await page.click('#knowledge-sources details.add-form:not(#url-source-form) summary');
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
  // URL sources: public groups and exact authenticated Wiki pages.
  await page.click('#knowledge-tab-sources');
  await page.click('#url-source-form summary');
  await page.fill('#url-source-id', 'public-guides');
  await page.fill('#url-source-addresses', 'https://docs.example.com/one\nhttps://docs.example.com/two');
  await page.click('#add-url-source'); await page.click('#save'); await page.waitForTimeout(300);
  check('a public URL source keeps all selected URLs', await page.evaluate(() => window.__ptaLastSave.map.resources.some(r => r.id === 'public-guides' && r.kind === 'urls' && r.urls.length === 2 && !r.enabled)));
  await page.click('#url-source-form summary');
  await page.fill('#url-source-id', 'create-user-wiki');
  await page.fill('#url-source-addresses', 'https://dev.azure.com/contoso/Payments/_wiki/wikis/Payments.wiki/12/Create-user');
  await page.selectOption('#url-source-wiki', 'wiki');
  await page.click('#add-url-source'); await page.click('#save'); await page.waitForTimeout(300);
  check('an exact Wiki source uses the configured connection without repository discovery', await page.evaluate(() => window.__ptaLastSave.map.resources.some(r => r.id === 'create-user-wiki' && r.tool.type === 'azure_devops_wiki' && r.tool.page_urls.length === 1 && r.tool.repository_docs === false && !r.enabled && !r.external_processing)));
  // Questions: add a question with explicit sources and keep it in the same map.
  await page.click('#knowledge-tab-questions');
  check('the initial question uses the activity record', await page.$$eval('#questions .resource', rows => rows.length) === 1 && /activity-record/.test(await page.textContent('#questions')));
  await page.click('#question-form summary');
  await page.fill('#question-text', 'Where is the deployment runbook?');
  await page.check('#question-sources input[value="create-user-wiki"]');
  await page.click('#add-question');
  await page.click('#save'); await page.waitForTimeout(300);
  check('a new question persists with its selected sources', await page.evaluate(() => window.__ptaLastSave.map.questions.some(q => q.text === 'Where is the deployment runbook?' && JSON.stringify(q.sources) === '["create-user-wiki"]')));
  await page.click('nav [data-tab="messages"]'); await page.waitForTimeout(300);
  const review = '#messages-list details[data-resource="chats/19:review/messages/1010"]';
  await page.locator(`${review} > summary`).click();
  await page.click(`${review} .question-association summary`);
  await page.selectOption(`${review} .question-association select`, 'daily-activities');
  await page.click(`${review} button:has-text("Guardar asociación")`); await page.waitForTimeout(400);
  check('an unanswered message is associated without being sent', await page.isVisible(`${review} .message-body`) && /Pregunta identificada/.test(await page.textContent(review)) && await page.evaluate(() => window.__ptaLastAssociation.question_id === 'daily-activities'));
  await page.click('nav [data-tab="knowledge"]'); await page.click('#knowledge-tab-questions');
  const daily = '#questions .resource:has-text("¿Qué hiciste hoy?")';
  await page.locator(`${daily} .resource-edit summary`).click();
  check('the reviewed wording appears in Questions', /Summary of your activities today/.test(await page.textContent(daily)));
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
  check('phone: five destinations in the tab bar', columns === 5, `${columns} columns`);
  await page.click('nav [data-tab="todo"]');
  await page.waitForTimeout(200);
  check('phone: To-Do has a direct tab and 44px link targets', await page.isVisible('#todo') && await page.$eval('.todo-item .icon-button', button => button.getBoundingClientRect().height >= 44));
  await page.evaluate(() => { window.__ptaDetailDelay = 800; });
  const headerHeight = await page.locator('.sidebar').evaluate(node => node.getBoundingClientRect().height);
  await page.click('.todo-item:first-child .todo-select');
  check('phone: header stays compact during detail loading', await page.locator('.sidebar').evaluate(node => node.getBoundingClientRect().height) === headerHeight && await page.isVisible('#todo-detail-progress'));
  await page.waitForSelector('#todo-detail-title');
  await page.evaluate(() => { window.__ptaDetailDelay = 0; });
  check('phone: detail fits the screen and has touch-sized navigation', await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth) && await page.$eval('#todo-back', button => button.getBoundingClientRect().height >= 44));
  await page.selectOption('#todo-detail-content .todo-relation-filter select', 'Task');
  await page.locator('#todo-detail-content .todo-list .todo-select').nth(1).tap();
  await page.waitForFunction(() => window.__ptaLastDetail?.id === 4503);
  check('phone: child tasks open inside the same detail and fit the viewport', await page.evaluate(() => window.__ptaLastDetail?.method === 'get_azure_task' && document.documentElement.scrollWidth <= window.innerWidth));
  await page.tap('#todo-back');
  check('phone: child back keeps the HU relation filter', await page.inputValue('#todo-detail-content .todo-relation-filter select') === 'Task');
  await page.screenshot({ path: `${out}/phone-todo-detail.png`, fullPage: true });
  await page.click('#todo-back');
  await page.click('#nav-more');
  check('phone: More reveals Knowledge and Test chat', await page.isVisible('nav [data-tab="knowledge"]') && await page.isVisible('nav [data-tab="chat"]'));
  await page.keyboard.press('Tab');
  check('phone: Tab enters the More disclosure in reading order', await page.evaluate(() => document.activeElement?.dataset.tab === 'agent-history'));
  await page.keyboard.press('Escape');
  check('phone: Escape closes More and restores focus', await page.getAttribute('#nav-more', 'aria-expanded') === 'false' && await page.evaluate(() => document.activeElement.id === 'nav-more'));
  await page.click('#nav-more');
  await page.click('nav [data-tab="knowledge"]');
  check('phone: secondary navigation closes after selecting a section', await page.isVisible('#knowledge') && await page.getAttribute('#nav-more', 'aria-expanded') === 'false');
  const box = await page.locator('nav .nav-settings').boundingBox();
  check('phone: Settings sits in the title bar', box && box.y < 60 && box.width >= 44, JSON.stringify(box));
  await page.click('nav .nav-settings');
  check('phone: Settings opens', await page.isVisible('#settings-tabs'));
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  check('phone: no horizontal page scroll', !overflow);
  const tabsScroll = await page.$eval('#settings-tabs', n => n.scrollWidth > n.clientWidth);
  check('phone: Settings sections all in sight, none scrolls sideways', !tabsScroll);
  await page.click('nav [data-tab="registration"]');
  await page.waitForTimeout(400);
  const days = await page.$$eval('#activity-log .card strong', h => h.map(x => x.textContent));
  check('phone: History lists recorded days including an empty day, newest first', days.length === 5, days.join(' | '));
  check('phone: History counts recorded days', /5 días/.test(await page.textContent('#activity-log-total')), await page.textContent('#activity-log-total'));
  await page.tap('#activity-log .card:first-child');
  await page.waitForSelector('#activity-day-tasks .todo-select');
  check('phone: day shows its existing task and advisory hours warning', /4502/.test(await page.textContent('#activity-day-tasks')) && await page.isVisible('#activity-day-warning'));
  check('phone: task has implement, original-link and copy actions', await page.locator('#activity-day-tasks .icon-button:visible').count() === 3 && await page.locator('#activity-day-tasks button[aria-label="Copiar enlace del work item"]').count() === 1);
  await page.tap('#activity-day-tasks .todo-select');
  await page.waitForSelector('#activity-detail-title');
  check('phone: task opens the shared detail through the task tool', await page.evaluate(() => window.__ptaLastDetail?.id === 4502 && window.__ptaCalls.includes('get_azure_task')));
  await page.evaluate(axeSource);
  const taskViolations = await scan(page);
  check('phone: task detail passes axe WCAG 2.2 AA', taskViolations.length === 0, JSON.stringify(taskViolations));
  await page.selectOption('#activity-detail-content .todo-relation-filter select', 'Task');
  await page.locator('#activity-detail-content .todo-list .todo-select').nth(1).tap();
  await page.waitForFunction(() => window.__ptaLastDetail?.id === 4503);
  check('phone: Activities opens related tasks through the same detail', (await page.textContent('#activity-detail-title')).includes('#4503') && await page.evaluate(() => window.__ptaLastDetail?.method === 'get_azure_task'));
  await page.tap('#activity-task-back');
  check('phone: Activities child back restores the original task and filter', (await page.textContent('#activity-detail-title')).includes('#4502') && await page.inputValue('#activity-detail-content .todo-relation-filter select') === 'Task');
  await page.tap('#activity-task-back');
  check('phone: back restores task focus', await page.evaluate(() => document.activeElement?.matches('#activity-day-tasks .todo-select')));
  await page.tap('#activity-days-back');
  check('phone: back restores day focus', await page.evaluate(() => document.activeElement?.matches('#activity-log .card')));
  await page.tap('#activity-log .card:last-child');
  await page.waitForTimeout(200);
  check('phone: saved empty day is visible', /No se registraron tareas/.test(await page.textContent('#activity-day-tasks')));
  await page.tap('#activity-days-back');
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
  check('phone: recorded day list remains available', await page.isVisible('#activity-master'));
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
for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport } });
  await page.evaluate(axeSource);
  await page.click('nav [data-tab="todo"]');
  await page.locator('#todo-list button[aria-label="Implementar work item"]').first().click();
  await page.waitForSelector('#implementation-provider option', { state: 'attached' });
  check(`${label}: implementation opens before starting and offers both machines`, await page.locator('#implementation-machine option').count() === 2 && await page.locator('.implementation-footer button').last().isDisabled());
  await page.fill('#implementation-context', 'Implement the receipt flow and check the phone layout.');
  const chooseDirectories = async () => {
    await page.getByRole('button', { name: '+ Elegir directorios', exact: true }).click();
    await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
    await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).check();
    await page.getByLabel('Seleccionar el directorio /work/documentation', { exact: true }).check();
    check(`${label}: directory sheet is modal, bounded and has no null text`, await page.locator('dialog.directory-picker').evaluate(node => node.open && node.getBoundingClientRect().height <= innerHeight && !node.textContent.includes('null')));
    check(`${label}: remote directory inspector passes axe`, (await scan(page)).length === 0);
    check(`${label}: parent navigation is the first directory row`, await page.locator('.directory-picker-list > :first-child button').textContent() === '../');
    await page.locator('.directory-picker-parent').click();
    await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
    check(`${label}: selections survive parent navigation`, await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).isChecked());
    await page.getByRole('button', { name: 'Usar los directorios seleccionados', exact: true }).click();
  };
  await chooseDirectories();
  check(`${label}: multiple directories are selected and add remains available`, await page.locator('.implementation-paths code').count() === 2 && await page.getByRole('button', { name: '+ Elegir directorios', exact: true }).isVisible());
  await page.selectOption('#implementation-machine', '22222222-2222-4222-8222-222222222222');
  check(`${label}: switching machines clears paths from the previous machine`, await page.locator('.implementation-paths code').count() === 0);
  await page.waitForFunction(() => !document.querySelector('#implementation-machine').disabled);
  await chooseDirectories();
  await page.locator('.implementation-footer button').last().click();
  await page.waitForSelector('.implementation-run input');
  check(`${label}: parent work item produces an editable child plan`, await page.evaluate(() => window.__ptaLastImplementation.item.task === false && window.__ptaLastImplementation.directories.length === 2));
  const violations = await scan(page);
  check(`${label}: implementation form and plan pass WCAG 2.2 AA`, violations.length === 0, JSON.stringify(violations));
  check(`${label}: implementation fits viewport`, await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await page.screenshot({ path: `${out}/${label}-implementation-plan.png`, fullPage: true });
  await page.getByRole('button', { name: 'Crear los hijos seleccionados', exact: true }).click();
  check(`${label}: child creation waits for the reviewed confirmation`, await page.evaluate(() => !window.__ptaLastChildCreation));
  await page.getByRole('button', { name: 'Confirmar creación de hijos', exact: true }).click();
  await page.waitForFunction(() => window.__ptaLastChildCreation);
  check(`${label}: child creation includes only the reviewed plan`, await page.evaluate(() => window.__ptaLastChildCreation.confirmed && window.__ptaLastChildCreation.children.length === 1));
  await page.locator('.implementation-footer button').first().click();
  check(`${label}: cancel returns to the originating To-Do view`, await page.isVisible('#todo-master'));
  await page.waitForSelector('#todo-list button[aria-label="Abrir sesión de Codex"]');
  check(`${label}: a work item with a native session offers its session action`, await page.locator('#todo-list button[aria-label="Abrir sesión de Codex"]').count() === 1);
  check(`${label}: cancel restores the implementation trigger's keyboard focus`, await page.evaluate(() => document.activeElement?.matches('#todo-list button[aria-label="Implementar work item"]')));
  await page.locator('#todo-list .todo-select').first().press('Enter');
  await page.waitForSelector('#todo-detail-title');
  await page.selectOption('#todo-detail-content .todo-relation-filter select', 'Task');
  await page.locator('#todo-detail-content .todo-list .todo-select').first().click();
  await page.waitForFunction(() => window.__ptaLastDetail?.method === 'get_azure_task');
  await page.locator('#todo-detail-content .todo-detail-header button[aria-label="Implementar work item"]').click();
  await page.waitForSelector('#implementation-provider option', { state: 'attached' });
  check(`${label}: task detail opens direct implementation`, (await page.textContent('#implementation-content h2')).includes('Implementar el work item seleccionado'));
  await chooseDirectories();
  await page.locator('.implementation-footer button').last().click();
  await page.waitForSelector('.implementation-output');
  check(`${label}: direct task execution preserves the verified task classification`, await page.evaluate(() => window.__ptaLastImplementation.item.task === true));
  await page.click('nav [data-tab="settings"]');
  await page.click('#settings-tab-teams');
  await page.waitForSelector('#teams-machine-list input[type=radio]');
  check(`${label}: existing Teams machine is selected by default`, await page.locator('#teams-machine-list input:checked').inputValue() === '11111111-1111-4111-8111-111111111111');
  await page.check('#teams-machine-list input[value="22222222-2222-4222-8222-222222222222"]');
  await page.waitForFunction(() => window.__ptaTeamsMachine && !document.querySelector('#teams-machine-list input').disabled);
  check(`${label}: Teams machine is configured separately from implementation`, await page.evaluate(() => window.__ptaTeamsMachine === '22222222-2222-4222-8222-222222222222'));
  await page.evaluate(() => { window.__ptaTeamsMachineError = true; });
  await page.check('#teams-machine-list input[value="11111111-1111-4111-8111-111111111111"]');
  await page.waitForFunction(() => !document.querySelector('#teams-machine-list input').disabled);
  check(`${label}: failed Teams switch restores the saved radio choice`, await page.locator('#teams-machine-list input:checked').inputValue() === '22222222-2222-4222-8222-222222222222');
  await page.evaluate(() => { window.__ptaTeamsMachineError = false; });
  await page.click('#notice-close');
  await page.click('#settings-tab-models');
  check(`${label}: machine list shares the Teams radio selection`, await page.locator('#machine-list input:checked').inputValue() === '22222222-2222-4222-8222-222222222222');
  check(`${label}: only the selected Teams machine can start`, await page.getByRole('button', { name: 'Iniciar asistente en Preview server', exact: true }).isDisabled() && await page.getByRole('button', { name: 'Iniciar asistente en Preview Mac', exact: true }).isEnabled());
  await page.getByRole('button', { name: 'Iniciar asistente en Preview Mac', exact: true }).click();
  await page.waitForSelector('#machine-list button[aria-label="Detener asistente en Preview Mac"]');
  check(`${label}: assistant start targets the selected remote machine`, await page.evaluate(() => window.__ptaLastMachineAssistant.machine === '22222222-2222-4222-8222-222222222222' && window.__ptaLastMachineAssistant.action === 'start'));
  await page.getByRole('button', { name: 'Detener asistente en Preview Mac', exact: true }).click();
  await page.waitForSelector('#machine-list button[aria-label="Iniciar asistente en Preview Mac"]');
  check(`${label}: assistant stop updates the row state`, await page.evaluate(() => window.__ptaLastMachineAssistant.action === 'stop'));
  await page.evaluate(() => { window.__ptaMachineAssistantError = true; });
  const callsBeforeFailure = await page.evaluate(() => window.__ptaMachineAssistantCalls);
  await page.getByRole('button', { name: 'Iniciar asistente en Preview Mac', exact: true }).click();
  await page.waitForFunction(() => !document.querySelector('#machine-list button[aria-label="Iniciar asistente en Preview Mac"]').disabled);
  check(`${label}: uncertain assistant control refreshes status without retrying`, await page.evaluate(before => window.__ptaMachineAssistantCalls === before + 1, callsBeforeFailure));
  await page.evaluate(() => { window.__ptaMachineAssistantError = false; });
  await page.click('#notice-close');
  await page.click('#machine-roots-choose');
  await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
  await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).check();
  await page.keyboard.press('Escape');
  check(`${label}: cancelling root selection preserves saved roots and focus`, await page.locator('#machine-roots code').count() === 1 && await page.evaluate(() => document.activeElement?.id === 'machine-roots-choose'));
  await page.click('#machine-roots-choose');
  await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
  await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).check();
  await page.getByRole('button', { name: 'Usar los directorios seleccionados', exact: true }).click();
  await page.click('#machine-save');
  await page.waitForFunction(() => window.__ptaMachineSettings?.roots.includes('/work/application'));
  check(`${label}: machine root picker saves selected directories`, await page.evaluate(() => window.__ptaMachineSettings.roots.includes('/work/application')));
  if (!await page.isVisible('nav [data-tab="knowledge"]')) await page.click('#nav-more');
  await page.click('nav [data-tab="knowledge"]');
  await page.click('#knowledge-tabs [data-group="repositories"]');
  await page.locator('#repo-path-choose').evaluate(node => { node.closest('details').open = true; });
  await page.evaluate(() => { window.__ptaBrowseDelay = 300; });
  await page.click('#repo-path-choose');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(350);
  check(`${label}: a cancelled pending directory read stays closed`, await page.locator('.directory-picker').count() === 0);
  await page.evaluate(() => { window.__ptaBrowseDelay = 0; window.__ptaBrowseError = true; });
  await page.click('#repo-path-choose');
  await page.waitForSelector('.directory-picker [role="alert"]');
  check(`${label}: directory failures stay inside the sheet`, await page.isVisible('.directory-picker [role="alert"]'));
  await page.evaluate(() => { window.__ptaBrowseError = false; });
  await page.getByRole('button', { name: 'Reintentar', exact: true }).click();
  await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
  await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).check();
  await page.getByLabel('Seleccionar el directorio /work/documentation', { exact: true }).check();
  check(`${label}: local checkout picker allows one selected directory`, await page.locator('.directory-picker input:checked').count() === 1);
  await page.getByRole('button', { name: 'Usar los directorios seleccionados', exact: true }).click();
  check(`${label}: checkout path comes from the shared sheet`, await page.inputValue('#repo-path') === '/work/documentation');
  await page.locator('#repositories button[aria-label]').first().click();
  check(`${label}: existing checkout editor reuses the same sheet`, await page.isVisible('.directory-picker'));
  await page.keyboard.press('Escape');
  check(`${label}: implementation has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Request creation shares the implementation model controls and records native sessions.
if (flag !== '--axe-only') for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport } });
  await page.evaluate(axeSource);
  await page.evaluate(() => showTab('todo'));
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  check(`${label}: request requires context and a catalog destination`, await page.isDisabled('#request-create'));
  check(`${label}: Azure request exposes HU and bug radios`, await page.locator('input[name="request-kind"]').count() === 2);
  await page.selectOption('#request-target', '0');
  await page.fill('#request-context', 'Download receipts and verify their contents.');
  await page.check('input[name="request-kind"][value="bug"]');
  check(`${label}: request enables creation after model and context selection`, !await page.isDisabled('#request-create'));
  check(`${label}: request form WCAG 2.2 AA`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.screenshot({ path: `${out}/request-${label}.png`, fullPage: true });
  await page.click('#request-create');
  await page.waitForSelector('#request-result .implementation-run');
  const submitted = await page.evaluate(() => window.__ptaLastRequestCreation);
  check(`${label}: Azure bug creation uses the selected agent and project`, submitted.kind === 'bug' && submitted.target.provider === 'azure' && !!submitted.model && !!submitted.effort);
  check(`${label}: a submitted request cannot be sent again`, await page.isDisabled('#request-create'));
  if (label === 'desktop') {
    await page.locator('#request-result').getByRole('button', { name: 'Abrir sesión del agente', exact: true }).click();
    await page.waitForFunction(() => window.__ptaOpenedAgentSession);
    check('request session button opens its recorded native session', await page.evaluate(id => window.__ptaOpenedAgentSession?.id === id, submitted.id));
  }
  await page.evaluate(() => showTab('agent-history'));
  await page.waitForSelector('#agent-history-list .implementation-run');
  const history = await page.textContent('#agent-history-list');
  check(`${label}: history includes run and native session IDs`, history.includes(submitted.id) && history.includes('ID de sesión del agente'));
  check(`${label}: history WCAG 2.2 AA`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.screenshot({ path: `${out}/agent-history-${label}.png`, fullPage: true });
  await page.evaluate(() => showTab('todo'));
  await page.selectOption('#todo-provider', 'github');
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  check(`${label}: GitHub hides HU/bug choices and uses a closed repository selector`, !await page.isVisible('input[name="request-kind"]') && await page.locator('#request-target option').count() === 2);
  await page.selectOption('#request-target', '0');
  await page.fill('#request-context', 'Build receipt download support.');
  await page.click('#request-create');
  await page.waitForSelector('#request-result .implementation-run');
  check(`${label}: GitHub creates an Epic`, await page.evaluate(() => window.__ptaLastRequestCreation.kind === 'epic'));
  await page.evaluate(() => showTab('agent-history'));
  await page.waitForFunction(() => document.querySelectorAll('#agent-history-list .implementation-run').length === 2);
  await page.evaluate(() => showTab('todo'));
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  await page.getByRole('button', { name: 'Cancelar', exact: true }).click();
  check(`${label}: cancel returns to To-Do without another execution`, await page.isVisible('#todo-master') && await page.evaluate(() => window.__ptaLastRequestCreation.kind === 'epic'));
  await page.evaluate(() => { window.__ptaRequestWriteReady = false; });
  await page.selectOption('#todo-provider', 'azure');
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  await page.selectOption('#request-target', '0');
  await page.fill('#request-context', 'Synthetic request with a missing write credential.');
  check(`${label}: missing Azure write credential prevents a run and offers settings`, await page.isDisabled('#request-create') && await page.getByRole('button', { name: 'Configurar credencial de escritura de Azure', exact: true }).isVisible());
  check(`${label}: request and history have no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
await browser.close();
fs.writeFileSync(`${out}/flows.json`, JSON.stringify(results, null, 1));
const failed = results.filter(r => !r.ok).length;
console.log(`${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
