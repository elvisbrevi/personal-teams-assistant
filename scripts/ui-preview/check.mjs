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

// Restored public Microsoft identifiers appear without overwriting unsaved setup edits.
{
  const { context, page, errors } = await open();
  await page.click('nav [data-tab="settings"]');
  await page.click('#settings-tab-teams');
  await page.evaluate(async () => {
    window.__ptaSetMicrosoftSetup({ tenant_id: '11111111-1111-4111-8111-111111111111' });
    await refreshLive();
  });
  check('restored Microsoft setup appears in its field', await page.inputValue('#tenant-id') === '11111111-1111-4111-8111-111111111111');
  await page.fill('#client-id', '22222222-2222-4222-8222-222222222222');
  await page.evaluate(async () => {
    window.__ptaSetMicrosoftSetup({ client_id: '33333333-3333-4333-8333-333333333333' }, 'A Microsoft setup variable is invalid. Enter a real UUID in Variables.');
    await refreshLive();
  });
  check('Microsoft setup refresh preserves unsaved edits', await page.inputValue('#client-id') === '22222222-2222-4222-8222-222222222222');
  check('Microsoft setup conflicts are translated and visible', await page.locator('#teams-identity-error').isVisible() && await page.locator('#teams-identity-error').innerText().then(text => text.includes('UUID real')));
  check('Microsoft setup has no client errors', errors.length === 0, errors.join('; '));
  await context.close();
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

// An admitted ordinary account sees its own identity and never probes host CLI sessions.
{
  const { context, page, errors } = await open({ axe: true, context: { viewport: { width: 393, height: 852 }, isMobile: true } }, 'account');
  check('shared account Home explains personal execution', await page.locator('#azure-cli-status').textContent().then(text => text.includes('máquina personal')));
  check('shared account does not probe operator CLI sessions', await page.evaluate(() => !window.__ptaCalls.some(call => ['azure_cli_status', 'github_cli_status'].includes(call.method))));
  await page.evaluate(() => showTab('settings', 'panel-web-account'));
  check('shared account shows its own identity', await page.locator('#account-identity').innerText().then(text => text.includes('bob')));
  check('shared account explains API execution limits in Spanish', await page.locator('#account-execution-limit').isVisible() && await page.locator('#account-execution-limit').innerText().then(text => text.includes('configuración API')));
  check('native account view omits browser sign out', !await page.locator('#web-logout-settings').isVisible());
  await page.evaluate(axeSource);
  const violations = await page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'] } })).violations);
  check('shared account phone settings pass axe', violations.length === 0, JSON.stringify(violations));
  check('shared account has no client errors', errors.length === 0, errors.join('; '));
  await context.close();
}

// ---------------------------------------------------------------- axe scans (both versions)
const axeSummary = {};
const scan = page => page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] } })).violations
  .map(v => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, sample: v.nodes[0]?.target?.join(' ') })));
// The owner's local page: the same GUI over fetch with the owner's authority, not the portal's.
for (const [label, ctx] of [['desktop', {}], ['phone', { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true }], ['dark', { colorScheme: 'dark' }]]) {
  // The fragment of the address the sign-in page moves to carries the session key (synthetic).
  const { context, page, errors } = await open({ axe: true, context: ctx }, 'full#k=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
  check(`local page ${label}: the key leaves the address and every API call carries it`, await page.evaluate(() => window.location.hash === ''
    && window.__ptaLocalSessionKey === 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' && window.__ptaLocalRequests.every(r => r.key === 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')));
  check(`local page ${label}: picks the local transport, not the portal`, await page.evaluate(() => window.ptaTransport.local === true && window.ptaTransport.browser === false && !window.__TAURI__));
  check(`local page ${label}: every call carries the session cookie and the CSRF token`, await page.evaluate(() => window.__ptaLocalRequests.length > 0
    && window.__ptaLocalRequests.every(r => r.csrf === 'synthetic-csrf-token' && r.credentials === 'same-origin')));
  check(`local page ${label}: no portal account or sign out`, await page.locator('#web-account').isHidden() && await page.locator('#web-logout-settings').isHidden());
  await page.evaluate(() => invoke('activity_open'));
  check(`local page ${label}: activities open in the page`, await page.evaluate(() => tab === 'registration' && !window.__ptaCalls.includes('activity_open')));
  await page.evaluate(axeSource);
  for (const section of ['overview', 'settings']) {
    await page.evaluate(t => showTab(t), section);
    await page.waitForTimeout(300);
    const violations = await scan(page);
    check(`local page ${label}: ${section} passes axe`, violations.length === 0, JSON.stringify(violations));
  }
  check(`local page ${label}: no client errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

for (const [label, ctx] of [['desktop-light', {}], ['desktop-dark', { colorScheme: 'dark' }], ['phone', { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true }]]) {
  const { context, page } = await open({ axe: true, context: ctx });
  await page.evaluate(axeSource);
  for (const tab of ['overview', 'todo', 'agent-history', 'agent-guides', 'messages', 'registration', 'workflows', 'knowledge', 'chat', 'settings']) {
    await page.evaluate(t => showTab(t), tab);
    await page.waitForTimeout(300);
    // Home's parts in order and the guides' editors are scanned open.
    if (tab === 'overview') await page.evaluate(() => { document.querySelector('#status-more').open = true; });
    if (tab === 'agent-guides') await page.evaluate(() => { for (const d of document.querySelectorAll('#agent-guides details')) d.open = true; });
    // Every tab of the section's tab list (Settings sections, Activities and Knowledge views) is scanned.
    const list = { settings: '#settings-tabs', knowledge: '#knowledge-tabs', workflows: '#workflows-tabs' }[tab];
    const groups = list ? await page.$$eval(`${list} [role=tab]`, tabs => tabs.map(t => t.dataset.group)).catch(() => []) : [];
    for (const group of groups.length ? groups : [null]) {
      if (group) { await page.evaluate(([l, g]) => selectTab($(l), g), [list, group]); await page.waitForTimeout(250); }
      // Knowledge editors are scanned open.
      if (tab === 'knowledge' || tab === 'workflows') await page.evaluate(t => { for (const d of document.querySelectorAll(`#${t} details`)) d.open = true; }, tab);
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
  check('nav order: daily work first, Settings last', JSON.stringify(order) === JSON.stringify(['overview', 'todo', 'messages', 'registration', 'workflows', 'knowledge', 'chat', 'settings']), order.join(','));
  await page.click('nav [data-tab="todo"]');
  await page.waitForTimeout(200);
  check('To-Do initially requests ten Azure HUs', await page.evaluate(() => window.__ptaLastTodo?.provider === 'azure' && window.__ptaLastTodo?.limit === 10) && await page.locator('.todo-item').count() === 10);
  check('To-Do includes states without assigned items', await page.locator('#todo-state option[value="Awaiting validation"]').count() === 1);
  check('a To-Do card shows the two dates nearest to today', await page.locator('.todo-item:first-child .todo-dates dt').count() === 2);
  await page.click('.todo-item:first-child .todo-select');
  await page.waitForSelector('#todo-detail-title');
  const detailDates = await page.locator('#todo-detail-content .todo-detail-header .todo-dates dt').allTextContents();
  check('the To-Do detail keeps when it was created and every date', detailDates.includes('Creada') && detailDates.includes('Inicio de desarrollo'), detailDates.join(', '));
  await page.click('#todo-back');
  await page.waitForSelector('.todo-item');
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
  check('HU detail includes assignment, acceptance criteria, evidence, discussion and original link', (await page.textContent('#todo-detail-content')).includes('Ana Pérez') && await page.getByRole('heading', { name: 'Criterios de aceptación' }).count() === 1 && (await page.textContent('#todo-detail-content')).includes('Luis Soto') && await page.locator('#todo-detail-content .todo-detail-header .icon-button:visible').count() === 4 /* Implement, original link, copy, and #5000's workflow run */ && await page.locator('#todo-detail-content .todo-detail-header button[aria-label="Copiar enlace del work item"]').count() === 1);
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
  check('Azure bug filter reaches the tool and keeps only bugs', await page.evaluate(() => window.__ptaLastTodo?.kind === 'bug') && await page.locator('.todo-item').count() === 6);
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
  check('Every type of the process is offered as a filter', await page.locator('#todo-kind optgroup option').count() === 4 && await page.locator('#todo-kind option[value="type:Task"]').count() === 1);
  await page.selectOption('#todo-kind', 'type:Development Bug');
  await page.waitForTimeout(200);
  check('An exact type reaches the tool instead of a group', await page.evaluate(() => window.__ptaLastTodo?.type === 'Development Bug' && !window.__ptaLastTodo?.kind) && await page.locator('.todo-item').count() === 1 && await page.inputValue('#todo-kind') === 'type:Development Bug');
  await page.selectOption('#todo-kind', 'task');
  await page.waitForTimeout(200);
  check('The task group reaches the tool', await page.evaluate(() => window.__ptaLastTodo?.kind === 'task') && await page.isVisible('#todo-list .empty'));
  await page.selectOption('#todo-kind', '');
  await page.selectOption('#todo-state', 'Finished');
  await page.waitForTimeout(200);
  check('To-Do state filter reaches the backend tool', await page.evaluate(() => window.__ptaLastTodo?.state === 'Finished') && await page.locator('.todo-item').count() === 7);
  await page.selectOption('#todo-state', 'Awaiting validation');
  await page.waitForTimeout(200);
  check('To-Do has a filtered empty state', await page.isVisible('#todo-list .empty'));
  await page.selectOption('#todo-state', '');
  await page.waitForTimeout(200);
  check('To-Do offers more while the list has more', await page.isVisible('#todo-more') && await page.locator('.todo-item').count() === 10);
  await page.click('#todo-more');
  await page.waitForFunction(() => document.querySelectorAll('.todo-item').length === 15);
  check('Show more asks the backend tool for the next step and continues at the first new item', await page.evaluate(() => window.__ptaLastTodo?.limit === 25
    && document.activeElement === document.querySelectorAll('.todo-item')[10].querySelector('.todo-select')) && !(await page.isVisible('#todo-more')));
  await page.selectOption('#todo-state', 'Finished');
  await page.waitForTimeout(200);
  check('a new filter starts again at the first step', await page.evaluate(() => window.__ptaLastTodo?.limit === 10));
  await page.selectOption('#todo-state', '');
  await page.waitForTimeout(200);
  await page.evaluate(() => { window.__ptaTodoError = true; });
  await page.click('#todo-refresh');
  await page.waitForTimeout(200);
  check('To-Do reports read errors inline and clears old items', await page.isVisible('#todo-problem') && await page.locator('.todo-item').count() === 0);
  await page.evaluate(() => { window.__ptaTodoError = false; window.__ptaTodoDelay = 250; });
  await page.click('#todo-refresh');
  await page.selectOption('#todo-provider', 'github');
  await page.waitForTimeout(400);
  check('To-Do ignores an older Azure response after switching provider', await page.locator('.todo-item').count() === 1 && (await page.textContent('#todo-list')).includes('Billing platform'));
  check('GitHub offers its native issue states, and a card without recorded dates says when it was created', await page.locator('#todo-state option[value="closed"]').count() === 1
    && (await page.locator('.todo-item .todo-dates dt').allTextContents()).join() === 'Creada', (await page.locator('.todo-item .todo-dates dt').allTextContents()).join());
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
  check('Home folds the parts in order into one line', !(await page.isVisible('#tiles')) && !(await page.isVisible('#tiles-fine .status-row'))
    && (await page.textContent('#status-more-summary')).includes('Todo en orden: 10 partes'), await page.textContent('#status-more-summary'));
  await page.click('#status-more-summary');
  await page.click('#tiles-fine .status-row:nth-child(1)');
  await page.waitForTimeout(300);
  check('status row opens Settings › Microsoft Teams', await page.isVisible('#panel-teams') && await page.getAttribute('#settings-tab-teams', 'aria-selected') === 'true');
  // Conditional fields
  check('allowed chats hidden while discovering all chats', !(await page.isVisible('#allowed-chats-field')));
  await page.click('#discover-chats');
  check('allowed chats shown once discovery is off', await page.isVisible('#allowed-chats-field'));
  check('edited section is flagged', await page.isVisible('#settings-tab-teams .flag.edited'));
  check('save bar appears with unsaved changes', await page.isVisible('#savebar'));
  await page.click('#discover-chats');
  check('reception is marked as this computer\'s and reading on request as the account\'s',
    (await page.textContent('#teams-reception legend')).includes('Este computador') && (await page.textContent('#teams-reading legend')).includes('Cuenta'));
  check('the account\'s readable chats are shown while not reading all chats',
    await page.isVisible('#reading-chats-field') && await page.isChecked('#reading-personal-chat') && (await page.inputValue('#reading-chats')).includes('19:abc@thread.v2'));
  await page.click('#reading-all-chats');
  check('reading all chats hides the personal chat and the list', !(await page.isVisible('#reading-chats-field')) && !(await page.isVisible('#reading-personal-chat-field')));
  await page.click('#reading-all-chats');
  // A change saved underneath the window (an account synchronization) waits while there are unsaved edits…
  await page.evaluate(async () => {
    window.__ptaChangeProfile(s => { s.config.teams_reading = { all_chats: true, personal_chat: true, chats: [] }; });
    await pollLive();
  });
  check('a synchronized change never replaces unsaved edits', !(await page.isChecked('#reading-all-chats')) && await page.isVisible('#savebar'));
  // …and reloads the form once there are none.
  await page.click('#discard');
  await page.evaluate(async () => {
    window.__ptaChangeProfile(s => { s.config.teams_reading = { all_chats: false, personal_chat: false, chats: ['19:synced@thread.v2'] }; });
    await pollLive();
  });
  check('a synchronized change shows in an open window without unsaved edits',
    !(await page.isChecked('#reading-all-chats')) && !(await page.isChecked('#reading-personal-chat')) && (await page.inputValue('#reading-chats')) === '19:synced@thread.v2' && !(await page.isVisible('#savebar')));
  // Keyboard tabs
  await page.focus('#settings-tab-teams');
  await page.keyboard.press('ArrowRight');
  check('ArrowRight selects the next settings section', await page.getAttribute('#settings-tab-models', 'aria-selected') === 'true' && await page.evaluate(() => document.activeElement.id) === 'settings-tab-models');
  await page.keyboard.press('End');
  check('End selects the last section', await page.getAttribute('#settings-tab-advanced', 'aria-selected') === 'true');
  // Connect an agent: without a portal there is no address, only how to use the local server.
  await page.waitForTimeout(200);
  check('without a portal, Connect an agent explains the local server instead of an address',
    await page.isHidden('#agent-endpoint') && await page.isVisible('#agent-endpoint-missing'));
  await page.evaluate(async () => { await invoke('machine_save', { coordinator: 'https://assistant.example.com' }); await loadAgentEndpoint(); });
  check('with a portal, Connect an agent shows its /mcp address with a copy action and how to add it',
    await page.textContent('#agent-endpoint-url') === 'https://assistant.example.com/mcp'
      && await page.isVisible('#agent-endpoint-copy')
      && (await page.textContent('#agent-step-claude')).includes('claude mcp add --transport http personal-teams-assistant https://assistant.example.com/mcp'));
  await page.evaluate(async () => { await invoke('machine_save', { coordinator: null }); await loadAgentEndpoint(); });
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
  // An effort field by type: added as a row, sent as a map; an incomplete row is refused first.
  await page.evaluate(() => { document.querySelector('#registration-effort-types').closest('details').open = true; });
  await page.click('#registration-effort-type-add');
  await page.fill('#registration-effort-types .effort-type:last-child [data-kind]', 'Bug');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('a type without its effort field is refused before saving', (await page.textContent('#notice-text')).includes('campo con horas reales'), await page.textContent('#notice-text'));
  await page.fill('#registration-effort-types .effort-type:last-child [data-field]', 'Custom.EsfuerzoReal');
  await page.click('#save');
  await page.waitForTimeout(400);
  const saved = await page.evaluate(() => window.__ptaLastSave?.config?.activity_registration);
  check('save sends the effort field by type', JSON.stringify(saved?.effort_fields) === '{"Bug":"Custom.EsfuerzoReal"}', JSON.stringify(saved?.effort_fields));
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
  // Organization: its name, documenting sources and entries, then the guides with their fields and the
  // implementation agents' instructions, all saved with the map.
  await page.click('#knowledge-tab-organization');
  check('the organization shows its entry', await page.isVisible('#organization-entries .resource:has-text("Code review")'));
  check('the organization names its documenting source', await page.isChecked('#organization-sources input[value="handbook"]'));
  check('the organization offers its source', await page.isVisible('#organization-offer'));
  await page.fill('#organization-name', 'Contoso Ltd');
  await page.click('#entry-form summary');
  await page.fill('#entry-id', 'layers');
  await page.selectOption('#entry-kind', 'architecture');
  await page.fill('#entry-title', 'Service layers');
  await page.fill('#entry-text', 'Handlers call services; services call repositories.');
  await page.click('#add-entry');
  await page.click('#save'); await page.waitForTimeout(300);
  check('the organization is saved with its new entry', await page.evaluate(() => {
    const o = window.__ptaLastSave?.map?.organization;
    return o?.name === 'Contoso Ltd' && o.sources.includes('handbook') && o.entries.some(e => e.id === 'layers' && e.kind === 'architecture');
  }));
  check('Organization no longer holds the guides or the agent instructions', await page.locator('#knowledge #guides, #knowledge #agent-prompts').count() === 0
    && (await page.textContent('#organization-summary')).trim() === '2 entradas', await page.textContent('#organization-summary'));
  await page.click('nav [data-tab="todo"]');
  await page.click('#todo-agent-guides');
  check('To-Do opens Guides and instructions and stays the current section', await page.isVisible('#agent-guides') && await page.getAttribute('nav [data-tab="todo"]', 'aria-current') === 'page'
    && await page.evaluate(() => document.activeElement?.matches('#agent-guides h1')));
  check('work item guides list the task guide', await page.isVisible('#guides .resource:has(code:text-is("Task"))'));
  await page.click('#guide-form summary');
  await page.fill('#guide-id', 'support');
  await page.fill('#guide-type', 'Support');
  await page.click('#add-guide');
  const support = page.locator('#guides > .resource').last();
  await support.locator('fieldset > .actions button').click();
  await support.locator('fieldset .list details input').first().fill('Custom.Kind');
  await page.click('#save'); await page.waitForTimeout(300);
  check('a new guide is saved with its field', await page.evaluate(() => window.__ptaLastSave.map.work_item_guides.some(g => g.id === 'support' && g.type === 'Support' && g.provider === 'azure' && g.fields[0]?.reference === 'Custom.Kind')));
  const plan = page.locator('#agent-prompts > .resource').first();
  check('agent instructions show the built-in prompts, the director\'s included', await page.$$eval('#agent-prompts > .resource', rows => rows.length) === 4 && await page.locator('#agent-prompts [data-prompt="direct"]').count() === 1);
  await plan.locator('summary').click();
  await plan.locator('textarea').fill('Plan at most three children.');
  await page.click('#save'); await page.waitForTimeout(300);
  check('a changed agent prompt is saved', await page.evaluate(() => window.__ptaLastSave.map.agent_prompts.some(p => p.id === 'plan' && p.instructions === 'Plan at most three children.')));
  await plan.locator('.actions button').click();
  await page.click('#save'); await page.waitForTimeout(300);
  check('restoring the default removes the change', await page.evaluate(() => !window.__ptaLastSave.map.agent_prompts.some(p => p.id === 'plan')));
  await page.click('#agent-guides-back');
  check('Back returns to To-Do, on the button that opened the guides', await page.isVisible('#todo-master') && await page.evaluate(() => document.activeElement?.id === 'todo-agent-guides'));
  await page.evaluate(() => showTab('knowledge'));
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
  // Message style: hidden while off; a combination fills the colors, the preview follows and Save sends the section.
  const { context, page, errors } = await open({ axe: true });
  await page.evaluate(() => showTab('settings', 'panel-message-style'));
  check('message style fields stay hidden while it is off', !await page.isVisible('#style-fields'));
  await page.check('#style-enabled');
  check('turning the message style on shows its fields and the default label', await page.isVisible('#style-preview')
    && await page.textContent('#style-preview-label') === 'Respuesta generada con IA' && await page.inputValue('#style-preset') === 'teams');
  await page.selectOption('#style-preset', 'green');
  await page.selectOption('#style-position', 'top');
  await page.fill('#style-label', 'Escrito por mi asistente');
  const preview = await page.evaluate(() => [document.querySelector('#style-preview').className, document.querySelector('#style-preview').style.getPropertyValue('--stripe')]);
  check('the preview follows the combination, position and label', preview[0] === 'teams-card band-good label-good at-top text-default label-size-small' && preview[1] === '#13a10e'
    && await page.textContent('#style-preview-label') === 'Escrito por mi asistente', preview.join(' | '));
  await page.evaluate(() => { document.querySelector('#style-custom').open = true; });
  await page.selectOption('#style-band', 'emphasis');
  check('changing one color makes the combination custom', await page.inputValue('#style-preset') === 'custom');
  await page.evaluate(axeSource);
  const scan = () => page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'] } })).violations);
  for (const label of ['default', 'subtle', 'accent', 'good', 'warning', 'attention']) {
    await page.selectOption('#style-label-color', label);
    for (const band of ['accent', 'emphasis', 'good', 'warning', 'attention', 'none']) {
      await page.selectOption('#style-band', band);
      for (const theme of ['light', 'dark']) {
        await page.evaluate(choice => applyAppearance({ theme: choice }), theme);
        const violations = await scan();
        if (violations.length) check(`message style preview passes axe (${label} on ${band}, ${theme})`, false, JSON.stringify(violations));
      }
    }
  }
  check('message style settings pass axe in every color pair and theme', true);
  await page.selectOption('#style-label-color', 'good');
  await page.selectOption('#style-band', 'emphasis');
  await page.fill('#style-stripe-width', '20');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('a stripe wider than 16 px is refused before saving', await page.isVisible('#notice.error'));
  await page.fill('#style-stripe-width', '8');
  await page.selectOption('#style-text-size', 'large');
  await page.selectOption('#style-label-size', 'medium');
  const sized = await page.evaluate(() => {
    const card = document.querySelector('#style-preview');
    return [getComputedStyle(card).borderLeftWidth, card.classList.contains('text-large'), card.classList.contains('label-size-medium')];
  });
  check('the preview follows the stripe width and text sizes', JSON.stringify(sized) === JSON.stringify(['8px', true, true]), JSON.stringify(sized));
  await page.fill('#style-band-height', '12');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('a band lower than 24 px is refused before saving', await page.isVisible('#notice.error'));
  await page.fill('#style-band-height', '56');
  await page.fill('#style-emoji', 'IA');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('an emoji field with words is refused before saving', await page.isVisible('#notice.error'));
  await page.click('#style-emoji-choices button[data-emoji="✨"]');
  const band = await page.evaluate(() => [document.querySelector('#style-preview .teams-band').getBoundingClientRect().height,
    document.querySelector('#style-preview-icon').textContent, document.querySelector('#style-emoji-choices button[data-emoji="✨"]').getAttribute('aria-pressed')]);
  check('the preview follows the band height and the chosen emoji', Math.round(band[0]) === 56 && band[1] === '✨' && band[2] === 'true', JSON.stringify(band));
  await page.uncheck('#style-icon');
  check('the emoji field hides with the emoji', !await page.isVisible('#style-emoji') && !await page.isVisible('#style-preview-icon'));
  await page.check('#style-icon');
  await page.click('#save');
  await page.waitForTimeout(300);
  check('the message style is saved', await page.evaluate(() => JSON.stringify(window.__ptaLastSave?.config?.message_style)) === JSON.stringify({
    enabled: true, label: 'Escrito por mi asistente', position: 'top', accent: '#13A10E', band: 'emphasis', label_color: 'good', icon: true,
    emoji: '✨', band_height: 56, stripe_width: 8, text_size: 'large', label_size: 'medium' }));
  check('message style has no page errors', errors.length === 0, errors.join(' | '));
  await context.close();
}
{
  // Every palette passes axe in light and dark, on the sections with the most colors; the styles rotate
  // so each one meets every palette (the token pairs are checked one by one in test-ui-themes.cjs).
  for (const [label, ctx, tabs] of [['desktop', {}, ['overview', 'messages', 'settings']], ['phone', { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true }, ['overview', 'settings']]]) {
    const { context, page, errors } = await open({ axe: true, context: ctx });
    await page.evaluate(axeSource);
    const palettes = await page.$$eval('#palette-choice button', buttons => buttons.map(button => button.dataset.palette));
    const styles = await page.$$eval('#style-choice button', buttons => buttons.map(button => button.dataset.style));
    const failures = [];
    let turn = 0;
    for (const palette of palettes) {
      for (const theme of ['light', 'dark']) {
        const style = styles[turn++ % styles.length];
        await page.evaluate(look => applyAppearance(look), { theme, palette, style });
        for (const tab of tabs) {
          await page.evaluate(name => showTab(name), tab);
          await page.waitForTimeout(120);
          const violations = await page.evaluate(async () => (await axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21aa', 'wcag22aa'] } })).violations
            .map(v => `${v.id}: ${v.nodes[0]?.target?.join(' ')}`));
          if (violations.length) failures.push(`${palette}/${style}/${theme}/${tab}: ${violations.join(', ')}`);
        }
      }
    }
    check(`${label}: every palette passes axe in light and dark`, failures.length === 0, failures.join(' | '));
    const sideways = await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth);
    check(`${label}: Appearance does not scroll sideways`, !sideways);
    check(`${label}: palettes have no page errors`, errors.length === 0, errors.join(' | '));
    await context.close();
  }
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
  // Palette and style: each applies at once on this device, asks for no save and survives a reload.
  const look = () => page.evaluate(() => [document.documentElement.dataset.palette ?? null, document.documentElement.dataset.style ?? null,
    getComputedStyle(document.body).backgroundColor, localStorage.getItem('pta-palette'), localStorage.getItem('pta-style')]);
  const radius = () => page.evaluate(() => getComputedStyle(document.querySelector('#theme-choice button')).borderTopLeftRadius);
  check('the look starts as Paper and Stickers', JSON.stringify(await look()) === JSON.stringify([null, null, 'rgb(255, 246, 229)', null, null])
    && await page.getAttribute('#palette-choice [data-palette="paper"]', 'aria-pressed') === 'true'
    && await page.getAttribute('#style-choice [data-style="sticker"]', 'aria-pressed') === 'true' && await radius() === '99px', JSON.stringify(await look()));
  check('Appearance offers three styles and ten palettes', await page.locator('#style-choice button').count() === 3 && await page.locator('#palette-choice button').count() === 10);
  await page.click('#palette-choice [data-palette="catppuccin"]');
  await page.click('#style-choice [data-style="soft"]');
  check('a palette and a style apply right away without asking to save', JSON.stringify(await look()) === JSON.stringify(['catppuccin', 'soft', 'rgb(239, 241, 245)', 'catppuccin', 'soft'])
    && await radius() === '10px' && !(await page.isVisible('#savebar')), JSON.stringify(await look()));
  const chosenBars = await page.$$eval('meta[name="theme-color"]', metas => metas.map(m => m.content));
  check('the browser bar takes the chosen palette', chosenBars.every(color => color === 'rgb(239, 241, 245)'), chosenBars.join(' | '));
  // A choice previews itself: it wears its own palette or style whatever the page uses. Buttons ease
  // between colors, so the page settles first (a slow machine read the tail of the last mode change).
  await page.waitForTimeout(400);
  const previews = await page.evaluate(() => [getComputedStyle(document.querySelector('#palette-choice [data-palette="paper"]')).backgroundColor,
    getComputedStyle(document.querySelector('#palette-choice [data-palette="monokai"]')).backgroundColor,
    getComputedStyle(document.querySelector('#style-choice [data-style="sticker"] .look-demo')).borderTopWidth,
    getComputedStyle(document.querySelector('#style-choice [data-style="minimal"] .look-demo')).borderTopWidth]);
  check('each choice previews its own palette or style', JSON.stringify(previews) === JSON.stringify(['rgb(255, 246, 229)', 'rgb(246, 241, 238)', '2px', '1px']), previews.join(' | '));
  await page.click('#theme-choice [data-theme-choice="dark"]');
  await page.reload();
  await page.waitForTimeout(600);
  check('the chosen look survives a reload', JSON.stringify((await look()).slice(0, 3)) === JSON.stringify(['catppuccin', 'soft', 'rgb(24, 24, 37)'])
    && await page.evaluate(() => document.documentElement.dataset.theme) === 'dark', JSON.stringify(await look()));
  await page.evaluate(() => showTab('settings'));
  await page.click('#style-choice [data-style="minimal"]');
  check('Minimal draws the same controls with small corners and no section tile', await radius() === '6px' && !(await page.isVisible('#settings .page-tile')));
  await page.click('#palette-choice [data-palette="paper"]');
  await page.click('#style-choice [data-style="sticker"]');
  await page.click('#theme-choice [data-theme-choice="system"]');
  check('choosing the defaults again forgets the look', JSON.stringify(await look()) === JSON.stringify([null, null, 'rgb(255, 246, 229)', null, null]), JSON.stringify(await look()));
  await page.evaluate(() => { localStorage.setItem('pta-palette', 'no-such-palette'); localStorage.setItem('pta-style', 'no-such-style'); });
  await page.reload();
  await page.waitForTimeout(600);
  check('an unknown stored look falls back to the defaults', JSON.stringify((await look()).slice(0, 3)) === JSON.stringify([null, null, 'rgb(255, 246, 229)']), JSON.stringify(await look()));
  await page.evaluate(() => { localStorage.removeItem('pta-palette'); localStorage.removeItem('pta-style'); });
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
  check('fresh: setup step opens Settings › Variables', await page.getAttribute('#settings-tab-variables', 'aria-selected') === 'true' && await page.isVisible('#panel-variables'));
  check('fresh: credentials and Teams flagged', await page.isVisible('#settings-tab-variables .flag.warn') && await page.isVisible('#settings-tab-teams .flag.warn'));
  await context.close();
}
{
  const { context, page } = await open({ context: { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true } });
  const columns = await page.$eval('nav', n => getComputedStyle(n).gridTemplateColumns.split(' ').length);
  check('phone: five destinations in the tab bar', columns === 5, `${columns} columns`);
  await page.click('nav [data-tab="todo"]');
  await page.waitForTimeout(200);
  check('phone: To-Do has a direct tab and 44px link targets', await page.isVisible('#todo') && await page.$eval('.todo-item .icon-button', button => button.getBoundingClientRect().height >= 44));
  check('phone: a card title takes the full width, its actions below', await page.$eval('.todo-item', card => {
    const title = card.querySelector('h3').getBoundingClientRect(), actions = card.querySelector('.todo-actions').getBoundingClientRect(), box = card.getBoundingClientRect();
    return title.width >= box.width - 48 && actions.top >= title.bottom;
  }));
  await page.evaluate(() => { window.__ptaDetailDelay = 800; });
  const headerHeight = await page.locator('.sidebar').evaluate(node => node.getBoundingClientRect().height);
  await page.click('.todo-item:first-child .todo-select');
  check('phone: header stays compact during detail loading', await page.locator('.sidebar').evaluate(node => node.getBoundingClientRect().height) === headerHeight && await page.isVisible('#todo-detail-progress'));
  await page.waitForSelector('#todo-detail-title');
  await page.evaluate(() => { window.__ptaDetailDelay = 0; });
  check('phone: a detail title takes the full width, its actions below', await page.$eval('.todo-detail-header', header => {
    const title = header.querySelector('h2').getBoundingClientRect(), actions = header.querySelector('.todo-actions').getBoundingClientRect();
    return title.width >= header.getBoundingClientRect().width - 48 && actions.top >= title.bottom;
  }));
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
  check('phone: More reveals Workflows, Knowledge and Test chat', await page.isVisible('nav [data-tab="workflows"]') && await page.isVisible('nav [data-tab="knowledge"]') && await page.isVisible('nav [data-tab="chat"]'));
  await page.keyboard.press('Tab');
  check('phone: Tab enters the More disclosure in reading order', await page.evaluate(() => document.activeElement?.dataset.tab === 'workflows'));
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
  for (const g of ['general', 'teams', 'models', 'activity', 'variables', 'advanced']) {
    await page.evaluate(x => selectTab($('#settings-tabs'), x), g);
    await page.waitForTimeout(150);
    await page.screenshot({ path: `${out}/phone-settings-${g}.png`, fullPage: true });
  }
  await context.close();
}
{
  const { context, page } = await open();
  await page.click('nav [data-tab="settings"]');
  for (const g of ['general', 'teams', 'models', 'activity', 'variables', 'advanced']) {
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
  check(`${label}: planning children shows no full-permissions note`, await page.locator('#implementation-permissions').count() === 0);
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
  check(`${label}: the coding form says the agent runs with full permissions on that computer, with no extra confirmation`, await page.locator('#implementation-permissions').innerText().then(text => text.includes('permisos completos en ese computador')) && await page.locator('#implementation-content dialog, #implementation-content input[type=checkbox]').count() === 0);
  await page.fill('#implementation-context', 'Open the pull request. [synthetic prompt]');
  await chooseDirectories();
  await page.locator('.implementation-footer button').last().click();
  await page.waitForSelector('.implementation-output');
  check(`${label}: direct task execution preserves the verified task classification`, await page.evaluate(() => window.__ptaLastImplementation.item.task === true));
  const recorded = page.locator('.implementation-run').last();
  check(`${label}: the run detail shows its permission mode`, (await recorded.locator('.implementation-permission-mode').innerText()).startsWith('Permisos completos en ese computador ('));
  check(`${label}: a prompt the CLI still raised is shown with its text and its own state`, (await recorded.locator('.implementation-prompts').innerText()).includes('gh pr create --fill') && (await recorded.innerText()).includes('Detenida en un permiso'));
  const recordedViolations = await scan(page);
  check(`${label}: a run stopped on a permission prompt passes WCAG 2.2 AA`, recordedViolations.length === 0, JSON.stringify(recordedViolations));
  check(`${label}: the run detail fits the viewport`, await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  await page.screenshot({ path: `${out}/${label}-implementation-run.png`, fullPage: true });
  await page.click('nav [data-tab="settings"]');
  await page.click('#settings-tab-teams');
  await page.click('#teams-machine-open');
  check(`${label}: Teams settings lead to the one machine list`, await page.getAttribute('#settings-tab-machines', 'aria-selected') === 'true' && await page.locator('#panel-teams-machine').count() === 0);
  await page.waitForSelector('#machine-list input[type=radio]');
  check(`${label}: existing Teams machine is selected by default`, await page.locator('#machine-list input:checked').inputValue() === '11111111-1111-4111-8111-111111111111');
  check(`${label}: each machine shows its reported version and update state, an older one as unknown`, await page.locator('#machine-list').innerText().then(text => text.includes('Versión 0.11.5 · actualización 0.11.6 pendiente · se actualiza solo') && text.includes('Versión desconocida')));
  await page.check('#machine-list input[value="22222222-2222-4222-8222-222222222222"]');
  await page.waitForFunction(() => window.__ptaTeamsMachine && !document.querySelector('#machine-list input').disabled);
  check(`${label}: Teams machine is configured separately from implementation`, await page.evaluate(() => window.__ptaTeamsMachine === '22222222-2222-4222-8222-222222222222'));
  await page.evaluate(() => { window.__ptaTeamsMachineError = true; });
  await page.check('#machine-list input[value="11111111-1111-4111-8111-111111111111"]');
  await page.waitForFunction(() => !document.querySelector('#machine-list input').disabled);
  check(`${label}: failed Teams switch restores the saved radio choice`, await page.locator('#machine-list input:checked').inputValue() === '22222222-2222-4222-8222-222222222222');
  await page.evaluate(() => { window.__ptaTeamsMachineError = false; });
  await page.click('#notice-close');
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
  check(`${label}: a coordinator is not asked to sign in`, await page.locator('#account-chip').isHidden() && !await page.evaluate(() => $('#account-login-dialog').open)
    && await page.locator('#machine-connect-hint').isHidden() === false);
  // A computer outside any account: the sign-in screen opens by itself and the sidebar keeps the action.
  await page.evaluate(() => {
    window.__ptaReadiness = { ready: true, mode: 'standalone', client: 'window', local_owner: true, coordinating: false, coordinator: null, next: null,
      steps: [{ id: 'coordinator', scope: 'coordinator', required: false, state: 'optional', detail: 'This installation works on its own. To use it with your assistant account, choose your coordinator and sign in once.', facts: {}, actions: [] }],
      agent: { skill_version: '0.9.13' } };
    return loadReadiness();
  });
  await page.waitForFunction(() => $('#account-login-dialog').open);
  check(`${label}: a computer outside any account is offered the sign-in screen`, await page.locator('#account-login-adopt').isChecked() && await page.evaluate(() => document.activeElement?.id === 'account-login-url'));
  await page.evaluate(axeSource);
  check(`${label}: the sign-in screen has no accessibility violations`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.fill('#account-login-url', 'https://assistant.example.com');
  await page.click('#account-login-submit');
  await page.waitForFunction(() => document.querySelector('#machine-login-code')?.textContent === '1A2B-3C4D');
  check(`${label}: signing in starts browser sign-in and shows its code`, await page.isVisible('#machine-login-dialog') && await page.evaluate(() => window.__ptaMachineLogin.url === 'https://assistant.example.com' && window.__ptaMachineLogin.open === true));
  check(`${label}: machine sign-in dialog has no accessibility violations`, (await scan(page)).length === 0);
  await page.click('#machine-login-cancel');
  await page.waitForFunction(() => !window.__ptaMachineLoginPending && !document.querySelector('#machine-login-dialog').open);
  check(`${label}: cancelling machine sign-in withdraws it and trusts nothing`, await page.evaluate(() => window.__ptaCalls.includes('machine_login_cancel') && !window.__ptaCalls.includes('account_sync')));
  await page.locator('#account-chip').waitFor();
  check(`${label}: the sidebar says this computer is not signed in`, await page.locator('#account-chip-action').innerText().then(text => text.includes('Iniciar sesión'))
    && await page.locator('#account-chip-action').getAttribute('title').then(text => text.includes('Sin sesión iniciada')));
  await page.click('#account-chip-action');
  await page.waitForFunction(() => $('#account-login-dialog').open);
  check(`${label}: the sign-in screen remembers the portal address`, await page.inputValue('#account-login-url') === 'https://assistant.example.com');
  await page.click('#account-login-submit');
  await page.waitForFunction(() => document.querySelector('#machine-login-code')?.textContent === '1A2B-3C4D');
  await page.evaluate(() => { window.__ptaMachineLoginTrusted = true; });
  await page.locator('#notice.ok').getByText('Sesión iniciada').waitFor();
  check(`${label}: a trusted sign-in brings the account's settings and variables at once`, !await page.isDisabled('#machine-disconnect')
    && await page.evaluate(() => window.__ptaAccountSyncs?.at(-1)?.reset === true));
  await page.locator('#account-chip-action', { hasText: 'Sincronizar' }).waitFor();
  check(`${label}: once signed in the sidebar offers Synchronize`, await page.locator('#account-chip-title').innerText().then(text => text.includes('Sesión iniciada')));
  await page.click('#account-chip-action');
  await page.waitForFunction(() => window.__ptaAccountSyncs.length === 2);
  check(`${label}: Synchronize never replaces local settings`, await page.evaluate(() => !window.__ptaAccountSyncs.at(-1).reset));
  check(`${label}: enrollment leaves previous directory grants empty`, await page.locator('#machine-roots code').count() === 0);
  await page.click('#notice-close');
  await page.evaluate(() => {
    $('#machine-trust-name').textContent = 'Synthetic laptop'; $('#machine-trust-os').textContent = 'Windows';
    $('#machine-trust-code').textContent = '1A2B-3C4D'; $('#machine-trust-account').textContent = 'ana';
    $('#machine-trust-store-field').hidden = false; $('#machine-trust-store').checked = false;
    $('#machine-trust-dialog').showModal();
  });
  check(`${label}: the trust dialog has no accessibility violations`, (await scan(page)).length === 0);
  check(`${label}: the account store is offered unticked, with what it gives`, await page.evaluate(() => !$('#machine-trust-store').checked)
    && (await page.locator('#machine-trust-store-field').innerText()).includes('almacén de cuenta') && (await page.locator('#machine-trust-store-field').innerText()).includes('clave de contenido'));
  await page.evaluate(() => $('#machine-trust-dialog').close());
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
  await page.evaluate(() => { window.__ptaWindowsDrives = true; });
  await page.click('#repo-path-choose');
  await page.getByRole('button', { name: 'Explorar el directorio D:\\', exact: true }).click();
  await page.getByRole('button', { name: 'Explorar el directorio D:\\code', exact: true }).click();
  await page.getByLabel('Seleccionar el directorio D:\\code\\sag', { exact: true }).waitFor();
  const windowsSheet = await page.locator('dialog.directory-picker').innerText();
  check(`${label}: Windows paths read plainly in the location and rows`, windowsSheet.includes('D:\\code\\sag')
    && await page.locator('dialog.directory-picker .directory-picker-location').innerText() === 'D:\\code' && !windowsSheet.includes('\\\\?\\'));
  await page.getByLabel('Seleccionar el directorio D:\\code\\sag', { exact: true }).check();
  await page.getByRole('button', { name: 'Usar los directorios seleccionados', exact: true }).click();
  check(`${label}: a directory on another drive can be chosen`, await page.inputValue('#repo-path') === 'D:\\code\\sag');
  await page.evaluate(() => { window.__ptaWindowsDrives = false; });
  check(`${label}: implementation has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Request creation shares the implementation model controls and records native sessions.
if (flag !== '--axe-only') for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport, ...(label === 'phone' ? { isMobile: true, hasTouch: true, userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Version/18.0 Mobile/15E148 Safari/604.1' } : {}) } });
  await page.evaluate(axeSource);
  await page.evaluate(() => showTab('todo'));
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  check(`${label}: request requires context and a catalog destination`, await page.isDisabled('#request-create'));
  check(`${label}: Azure request exposes HU and bug radios`, await page.locator('input[name="request-kind"]').count() === 2);
  await page.click('#request-target');
  check(`${label}: destination sheet focuses its first name filter`, await page.locator('#request-target-search').evaluate(node => node === document.activeElement));
  check(`${label}: three recent projects precede every other project`, await page.locator('.destination-picker [data-recent] button').count() === 3 && await page.locator('.destination-picker button[value]').count() === 4 && (await page.locator('.destination-picker button[value]').first().getAttribute('value')) === '2');
  await page.fill('#request-target-search', 'rEcEiPtS');
  check(`${label}: Azure finds a project beyond the recent three`, await page.locator('.destination-picker button[value]').count() === 1 && (await page.textContent('.destination-picker button[value]')).includes('Receipts'));
  check(`${label}: destination sheet WCAG 2.2 AA`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.screenshot({ path: `${out}/destination-filter-${label}.png`, fullPage: true });
  await page.fill('#request-target-search', 'no-match');
  check(`${label}: empty filtering explains no matches`, await page.locator('.destination-picker button[value]').count() === 0 && (await page.textContent('.destination-picker')).includes('No hay destinos'));
  await page.keyboard.press('Escape');
  await page.waitForSelector('.destination-picker', { state: 'detached' });
  check(`${label}: cancelling the picker restores trigger focus and preserves the empty choice`, await page.locator('#request-target').evaluate(node => node === document.activeElement && node.value === ''));

  await page.click('#request-target');
  await page.locator('.destination-picker button[value="0"]').click();
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
  if (label === 'phone') {
    await page.locator('#agent-history-list').getByRole('button', { name: 'Abrir sesión del agente', exact: true }).first().click();
    await page.getByText('Este equipo no aparece en Codex', { exact: true }).click();
    await page.getByRole('button', { name: 'Vincular este teléfono con Codex', exact: true }).waitFor();
    await page.getByRole('button', { name: 'Vincular este teléfono con Codex', exact: true }).click();
    await page.getByText('TEST-CODE', { exact: true }).waitFor();
    check('PWA: Codex pairing is shown on demand without replacing the page', (await page.textContent('dialog[open]')).includes('TEST-CODE') && await page.evaluate(() => window.__ptaMobileSession.action === 'pair'));
    check('PWA: the session sheet never renders an absent mobile-copy hint as null', !(await page.textContent('dialog[open]')).includes('null'));
    check('PWA: mobile session sheet WCAG 2.2 AA', (await scan(page)).length === 0, JSON.stringify(await scan(page)));
    await page.screenshot({ path: `${out}/mobile-codex.png`, fullPage: true });
    await page.keyboard.press('Escape');
    await page.evaluate(() => { window.__ptaMobileProvider = 'claude'; });
    await page.evaluate(() => showMobileSession({ id: 'synthetic-run', machine: 'synthetic-machine', provider: 'claude', session_id: '11111111-1111-4111-8111-111111111111', state: 'done' }, document.querySelector('#agent-history-list button')));
    await page.getByRole('link', { name: 'Abrir en la app', exact: true }).waitFor();
    check('PWA: Claude offers the provider app link with a separate web fallback', await page.getByRole('link', { name: 'Abrir en la app', exact: true }).getAttribute('href') === 'claude://code/session_SyntheticMobile123' && await page.getByRole('link', { name: 'Abrir en la web', exact: true }).getAttribute('href') === 'https://claude.ai/code/session_SyntheticMobile123');
    check('PWA: Claude connection WCAG 2.2 AA', (await scan(page)).length === 0, JSON.stringify(await scan(page)));
    await page.screenshot({ path: `${out}/mobile-claude.png`, fullPage: true });
    await page.keyboard.press('Escape');
    await page.evaluate(() => { delete window.__ptaMobileProvider; });
  }
  await page.evaluate(() => showTab('todo'));
  await page.selectOption('#todo-provider', 'github');
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  check(`${label}: GitHub hides HU/bug choices and uses a closed repository selector`, !await page.isVisible('input[name="request-kind"]') && await page.locator('#request-target[aria-haspopup="dialog"]').count() === 1);
  await page.click('#request-target');
  check(`${label}: GitHub lists all public and private destinations`, await page.locator('.destination-picker button[value]').count() === 4);
  await page.fill('#request-target-search', 'PRIVATE');
  check(`${label}: GitHub filters repository names without changing the target`, await page.locator('.destination-picker button[value]').count() === 1 && (await page.textContent('.destination-picker button[value]')).includes('private-tools'));
  await page.keyboard.press('Escape');

  await page.click('#request-target');
  await page.locator('.destination-picker button[value="0"]').click();
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
  await page.evaluate(() => { window.__ptaGithubCliState = 'not_authenticated'; });
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  check(`${label}: GitHub CLI login is required and has a direct status action`, await page.isDisabled('#request-create') && await page.getByRole('button', { name: 'Consultar estado de GitHub CLI', exact: true }).isVisible());
  await page.getByRole('button', { name: 'Consultar estado de GitHub CLI', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#github-cli-status')?.textContent.includes('Sin autenticar'));
  check(`${label}: Home shows GitHub CLI login instructions`, (await page.textContent('#github-cli-status')).includes('gh auth login'));
  await page.evaluate(() => { window.__ptaGithubCliState = 'authenticated'; });
  await page.click('#github-cli-status');
  await page.waitForFunction(() => document.querySelector('#github-cli-status')?.textContent.includes('Autenticada'));
  check(`${label}: GitHub CLI refresh shows the authenticated account`, (await page.textContent('#github-cli-status')).includes('example'));
  await page.evaluate(() => showTab('todo'));
  await page.evaluate(() => { window.__ptaAzureCliState = 'not_authenticated'; });
  await page.selectOption('#todo-provider', 'azure');
  await page.click('#todo-create');
  await page.waitForSelector('#request-target');
  await page.click('#request-target');
  await page.locator('.destination-picker button[value="0"]').click();
  await page.fill('#request-context', 'Synthetic request without an authenticated Azure CLI session.');
  check(`${label}: an unauthenticated Azure CLI session prevents a run and links to status`, await page.isDisabled('#request-create') && await page.getByRole('button', { name: 'Consultar estado de Azure CLI', exact: true }).isVisible());
  await page.getByRole('button', { name: 'Consultar estado de Azure CLI', exact: true }).click();
  await page.waitForFunction(() => document.querySelector('#azure-cli-status')?.textContent.includes('Sin autenticar'));
  check(`${label}: Home shows the actual Azure CLI authentication state`, await page.isVisible('#azure-cli-status') && (await page.textContent('#azure-cli-status')).includes('iniciar sesión con Microsoft'));
  await page.click('#azure-cli-status');
  await page.waitForFunction(() => document.querySelector('#azure-cli-code')?.textContent === 'ABCD12345');
  check(`${label}: the status row starts Microsoft device sign-in`, await page.isVisible('#azure-cli-dialog') && await page.getAttribute('#azure-cli-link', 'href') === 'https://microsoft.com/devicelogin');
  check(`${label}: sign-in waits without blocking navigation back to the app`, await page.evaluate(() => window.__ptaAzureLoginPending) && (await page.textContent('#azure-cli-login-status')).includes('Esperando'));
  await page.click('#azure-cli-cancel');
  await page.waitForFunction(() => !window.__ptaAzureLoginPending);
  check(`${label}: cancelling stops sign-in and keeps the session unauthenticated`, !await page.isVisible('#azure-cli-dialog') && (await page.textContent('#azure-cli-status')).includes('Sin autenticar'));
  await page.click('#azure-cli-status');
  await page.waitForFunction(() => document.querySelector('#azure-cli-code')?.textContent === 'ABCD12345');
  await page.evaluate(axeSource);
  check(`${label}: Azure sign-in dialog has no accessibility violations`, (await scan(page)).length === 0);
  await page.evaluate(() => { window.__ptaAzureCliState = 'authenticated'; });
  await page.waitForFunction(() => !document.querySelector('#azure-cli-dialog').open);

  await page.waitForFunction(() => document.querySelector('#azure-cli-status')?.textContent.includes('Autenticada'));
  check(`${label}: Azure CLI status can be refreshed without editing credentials`, (await page.textContent('#azure-cli-status')).includes('ana@contoso.example'));
  await page.evaluate(() => { window.__ptaAzureCliState = 'unavailable'; loadAzureCliStatus(); });
  await page.waitForFunction(() => document.querySelector('#azure-cli-status')?.textContent.includes('Sesión no disponible'));
  await page.click('#azure-cli-status');
  await page.waitForFunction(() => document.querySelector('#azure-cli-code')?.textContent === 'ABCD12345');
  await page.click('#azure-cli-link');
  check(`${label}: native sign-in opens Microsoft through the host shell`, await page.evaluate(() => window.__ptaCalls.includes('open_azure_cli_login')));
  await page.evaluate(() => { window.__ptaAzureLoginError = true; });
  await page.waitForFunction(() => !document.querySelector('#azure-cli-dialog').open);
  await page.waitForFunction(() => !document.querySelector('#azure-cli-status').disabled && document.querySelector('#notice-text').textContent.includes('falló o caducó'));
  check(`${label}: failed sign-in is explained and can be retried`, (await page.textContent('#notice-text')).includes('falló o caducó') && !await page.isDisabled('#azure-cli-status'));
  await page.evaluate(() => { window.__ptaAzureLoginError = false; window.__ptaAzureCliState = 'not_installed'; loadAzureCliStatus(); });
  await page.waitForFunction(() => document.querySelector('#azure-cli-status')?.textContent.includes('No instalada'));
  const logins = await page.evaluate(() => window.__ptaCalls.filter(method => method === 'begin_azure_cli_login').length);
  await page.click('#azure-cli-status');
  check(`${label}: a missing CLI explains installation without attempting sign-in`, !await page.isVisible('#azure-cli-dialog') && await page.evaluate(() => window.__ptaCalls.filter(method => method === 'begin_azure_cli_login').length) === logins);
  check(`${label}: request and history have no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

// Account machine management reuses the same GUI and directory sheet at both sizes.
for (const [label, ctx] of [['machine-desktop', {}], ['machine-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.evaluate(() => showTab('settings', 'panel-machines'));
  check(`${label}: a link to the machines opens their Settings category`, await page.getAttribute('#settings-tab-machines', 'aria-selected') === 'true');
  // Connect this computer: only the portal shows its install command, for the visitor's system first.
  await page.waitForTimeout(150);
  check(`${label}: the local page offers no install command`, await page.isHidden('#machine-join'));
  await page.evaluate(async () => { window.ptaTransport.browser = true; await loadJoinCommand(); });
  await page.click('#machine-join summary');
  const joined = await page.textContent('#machine-join-command');
  check(`${label}: the portal shows a one-line install command with its own origin`, await page.isVisible('#machine-join-command') && joined.includes('--origin https://assistant.example.com') && joined.startsWith('curl --proto'));
  await page.getByRole('button', { name: 'Windows', exact: true }).click();
  check(`${label}: another system is selectable and pressed`, (await page.textContent('#machine-join-command')).startsWith('Invoke-RestMethod')
    && await page.getAttribute('#machine-join-system [data-system="windows"]', 'aria-pressed') === 'true'
    && await page.getAttribute('#machine-join-system [data-system="linux"]', 'aria-pressed') === 'false');
  await page.evaluate(() => loadJoinCommand());
  check(`${label}: a refresh keeps the chosen system`, (await page.textContent('#machine-join-command')).startsWith('Invoke-RestMethod'));
  await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async text => { window.__ptaCopiedJoinCommand = text; } } }));
  await page.click('#machine-join-copy');
  await page.locator('#notice', { hasText: 'Comando copiado' }).waitFor();
  check(`${label}: the command is copied exactly as shown`, await page.evaluate(() => window.__ptaCopiedJoinCommand === document.querySelector('#machine-join-command').textContent));
  check(`${label}: the install command stays within the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: Connect this computer passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.evaluate(async () => { window.ptaTransport.browser = false; await loadJoinCommand(); });
  await page.getByRole('button', { name: 'Administrar máquina: Preview Mac', exact: true }).click();
  const editor = page.getByRole('dialog', { name: 'Administrar máquina', exact: true });
  await editor.waitFor();
  await editor.getByLabel('Nombre de la máquina', { exact: true }).fill('Travel laptop');
  await editor.getByRole('button', { name: 'Elegir directorios', exact: true }).click();
  await page.getByRole('button', { name: 'Explorar el directorio /work', exact: true }).click();
  await page.locator('dialog[open] .directory-picker-parent').waitFor();
  check(`${label}: remote root sheet has parent navigation`, await page.locator('dialog[open] .directory-picker-parent').count() === 1);
  await page.getByLabel('Seleccionar el directorio /work/application', { exact: true }).check();
  await page.getByRole('button', { name: 'Usar los directorios seleccionados', exact: true }).click();
  check(`${label}: directory confirmation stages roots in the machine editor`, await editor.locator('code').innerText() === '/work/application');
  check(`${label}: editor and nested sheet preserve the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: machine editor passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.evaluate(() => { window.__ptaMachineUpdateError = true; });
  await editor.getByRole('button', { name: 'Guardar cambios', exact: true }).click();
  await page.waitForFunction(() => {
    const calls = window.__ptaCalls, edit = calls.lastIndexOf('machine_update');
    return edit >= 0 && calls.slice(edit + 1).includes('machine_list');
  });
  check(`${label}: uncertain edits refresh authoritative state without replay`, await page.evaluate(() => window.__ptaCalls.filter(method => method === 'machine_update').length === 1));
  await page.evaluate(() => { window.__ptaMachineUpdateError = false; });
  await editor.getByRole('button', { name: 'Guardar cambios', exact: true }).click();
  await page.waitForFunction(() => window.__ptaMachineUpdate?.name === 'Travel laptop' && !document.querySelector('dialog[aria-label="Administrar máquina"]'));

  check(`${label}: remote roots and name share the host operation`, await page.evaluate(() => window.__ptaMachineUpdate.roots[0] === '/work/application' && !window.__ptaLastMachineAssistant));
  check(`${label}: a machine whose version is unknown offers no update`, await page.locator('#machine-list button:has-text("Actualizar a")').count() === 0);
  await page.evaluate(() => { window.__ptaMachineOlder = true; return loadMachines(); });
  const upgrade = page.getByRole('button', { name: 'Actualizar a 0.11.6: Travel laptop', exact: true });
  await upgrade.waitFor();
  check(`${label}: an older machine offers its update to the newest known release`, (await upgrade.textContent()) === 'Actualizar a 0.11.6'
    && await page.getByRole('button', { name: /: Preview server$/ }).filter({ hasText: 'Actualizar a' }).count() === 0);
  await page.evaluate(axeSource);
  check(`${label}: the machine list with an update passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await upgrade.click();
  const confirm = page.getByRole('button', { name: 'Confirmar actualización: Travel laptop', exact: true });
  check(`${label}: a machine update requires its explicit second tap`, !await page.evaluate(() => window.__ptaMachineUpgrade) && await confirm.isVisible());
  await confirm.click();
  await page.waitForFunction(() => window.__ptaMachineUpgrade);
  check(`${label}: the machine update names only the machine`, await page.evaluate(() => JSON.stringify(window.__ptaMachineUpgrade) === JSON.stringify({ machine: '22222222-2222-4222-8222-222222222222' })));
  await page.waitForFunction(() => document.querySelector('#machine-list')?.textContent.includes('preparando una actualización'));
  check(`${label}: a machine preparing its update shows it and offers no second request`, await page.locator('#machine-list button:has-text("Actualizar a")').isDisabled());
  await page.evaluate(() => { window.__ptaMachineOffline = true; loadMachines(); });
  await page.getByRole('button', { name: 'Administrar máquina: Travel laptop', exact: true }).click();
  await editor.waitFor();
  check(`${label}: offline machine roots cannot be changed`, await editor.getByRole('button', { name: 'Elegir directorios', exact: true }).isDisabled());
  // The tap that opened the editor leaves the pointer on the edge of its footer, where a mouse's hover lift moves
  // the button away from it and back on every frame (a phone has no hover).
  await page.mouse.move(0, 0);
  await editor.getByRole('button', { name: 'Revocar máquina', exact: true }).click();
  check(`${label}: revocation requires its explicit second tap`, !await page.evaluate(() => window.__ptaMachineRevoked));
  await editor.getByRole('button', { name: 'Confirmar revocación', exact: true }).click();
  await page.waitForFunction(() => window.__ptaMachineRevoked);
  check(`${label}: revoked machine cannot be selected for Teams`, await page.locator('#machine-list input[value="22222222-2222-4222-8222-222222222222"]').isDisabled());
  check(`${label}: machine workflow has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Catalog synchronization: a connected installation shows where its copy stands; the sidebar synchronizes it.
for (const [label, ctx] of [['variable-sync-desktop', {}], ['variable-sync-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.evaluate(() => showTab('settings', 'panel-variables'));
  await page.locator('#variables .row').first().waitFor();
  check(`${label}: a catalog that is its own authority says it is current`, await page.locator('#variables-sync').innerText().then(text => text.includes('siempre están al día') && text.includes('Revisión 7')));
  check(`${label}: the button stays a plain refresh there`, await page.locator('#variables-refresh').innerText() === 'Actualizar');
  await page.evaluate(() => {
    window.__ptaVariableSync = { mode: 'mirror', state: 'synced', coordinator: 'https://assistant.example.test', revision: 12, synced_at: Math.floor(Date.now() / 1000) - 120, entries: 2, error: null, conflict: null };
    window.__ptaReadiness = { ready: true, mode: 'signed_in', client: 'window', local_owner: true, coordinating: false, coordinator: 'https://assistant.example.test', next: null,
      steps: [{ id: 'coordinator', scope: 'coordinator', required: true, state: 'done', detail: "This computer is signed in to your account's coordinator.", facts: {}, actions: [] }], agent: { skill_version: '0.9.13' } };
    return Promise.all([loadVariables(), loadReadiness()]);
  });
  await page.locator('#variables-refresh').click();
  check(`${label}: Refresh only rereads this computer's list`, await page.evaluate(() => !window.__ptaCalls.includes('variable_sync') && !window.__ptaCalls.includes('account_sync'))
    && await page.locator('#variables-refresh').innerText() === 'Actualizar');
  await page.locator('#account-chip-action').click();
  await page.locator('#variables-sync', { hasText: 'Revisión 13' }).waitFor();
  check(`${label}: the sidebar's Synchronize brings the catalog and reports the loaded revision`, await page.locator('#variables-sync').innerText().then(text => text.includes('Sincronizado con tu cuenta')));
  check(`${label}: the status is a live region`, await page.locator('#variables-sync').getAttribute('role') === 'status');
  await page.evaluate(() => { window.__ptaSyncFail = true; });
  await page.locator('#account-chip-action').click();
  await page.locator('#variables-sync', { hasText: 'Falló la sincronización' }).waitFor();
  check(`${label}: an outage keeps the last catalog and says so in Spanish`, await page.locator('#variables .row').count() === 2
    && await page.locator('#variables-sync').innerText().then(text => text.includes('Revisión 13') && text.includes('No se pudo contactar al coordinador')));
  await page.evaluate(() => { window.__ptaSyncFail = false; window.__ptaVariableSync = { ...window.__ptaVariableSync, state: 'conflict', error: null, conflict: 'API changed since it was loaded (it is now version 4). Inspect it before editing again.' }; return loadVariables(); });
  check(`${label}: a conflict is announced with advice`, await page.locator('#variables-sync').innerText().then(text => text.includes('chocó con otro más reciente') && text.includes('Revisa') === false && text.includes('revísala')));
  await page.click('#notice-close').catch(() => {});
  await page.locator('#account-chip-action').click();
  await page.locator('#variables-sync', { hasText: 'Sincronizado con tu cuenta' }).waitFor();
  check(`${label}: an explicit synchronization clears a conflict`, await page.locator('#variables-sync').innerText().then(text => !text.includes('chocó')));
  check(`${label}: synchronization keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: synchronization state passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: synchronization has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

// Account readiness: the ordered steps of a signed-in installation, each marked Account or This
// computer, with facts that keep configured, running, received and sent apart.
for (const [label, ctx] of [['readiness-desktop', {}], ['readiness-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.waitForFunction(() => readiness !== null);
  await page.evaluate(() => renderSteps(live));
  check(`${label}: a coordinator shows no account guidance`, await page.locator('#account-setup-panel').isHidden());
  check(`${label}: signing in is not repeated in the first-run checklist`, await page.locator('#setup-steps').innerText().then(text => !text.includes('Agregar este equipo a tu cuenta') && !text.includes('Iniciar sesión')));
  await page.evaluate(() => {
    const step = (id, scope, state, detail, facts = {}, required = true) => ({ id, scope, required, state, detail, facts, actions: [] });
    window.__ptaReadiness = {
      ready: false, mode: 'signed_in', client: 'window', local_owner: true, coordinator: 'https://assistant.example.test',
      next: { step: 'variables', state: 'todo', detail: 'Some variables you need are not stored yet. Type each value in the protected entry (Settings > Variables, or `pta variables entry complete`); it never goes through chat.', actions: [] },
      steps: [
        step('coordinator', 'coordinator', 'done', 'This computer is signed in to your account\'s coordinator.'),
        step('connection', 'coordinator', 'done', 'The coordinator answered and accepts this computer.', { standing: 'live', checked_at: Math.floor(Date.now() / 1000) - 60 }),
        step('machine', 'coordinator', 'done', 'This computer is on your account\'s machine list, which every signed-in client shows.', { machines: 3, online: 2 }),
        step('configuration', 'coordinator', 'done', 'Your account configuration is restored and up to date on this computer.', { loaded: { generation: 5 }, saved: { generation: 5 } }),
        step('variables', 'coordinator', 'todo', 'Some variables you need are not stored yet. Type each value in the protected entry (Settings > Variables, or `pta variables entry complete`); it never goes through chat.',
          { count: 2, expected: [{ name: 'DEPLOY_ENDPOINT', state: 'present', version: 3 }, { name: 'PARTNER_TOKEN', state: 'waiting' }, { name: 'SIGNING_KEY', state: 'missing' }] }),
        step('migration', 'this_machine', 'optional', 'Optional: review which existing credentials of this installation to move into your account. Nothing is moved until you confirm each one.', {}, false),
        step('providers', 'this_machine', 'attention', 'A model provider in the configuration is not installed or signed in on this computer. Nothing is installed or used in its place; install it and sign in yourself, then check again.', { providers: ['codex'] }, false),
        step('roots', 'this_machine', 'optional', 'No directory is selected for coding agents on this computer. Registration never grants access; choose the folders yourself with the directory selector.', { count: 0 }, false),
        step('teams', 'this_machine', 'todo', 'Teams is configured here but the assistant is stopped. Start it yourself when you are ready; nothing starts automatically.', { microsoft_signed_in: true, running: false, received: 4, sent: 1, last_received_at: Math.floor(Date.now() / 1000) - 3600 }, false),
        step('versions', 'this_machine', 'optional', 'A newer release is available. Updating is a separate, confirmed action.', { running: '0.9.13', installed: '0.9.13', available: '0.9.14' }, false),
      ] };
    return loadReadiness();
  });
  const panel = page.locator('#account-setup-panel');
  await panel.waitFor();
  const rows = panel.locator('#account-setup-steps > li');
  check(`${label}: every step of the guided path is listed in order`, await rows.count() === 10 && await rows.nth(0).innerText().then(text => text.includes('Coordinador e inicio de sesión')) && await rows.nth(9).innerText().then(text => text.includes('Versiones')));
  check(`${label}: account scope and this-computer scope are both labelled`, await panel.locator('#account-setup-steps .badge', { hasText: 'Cuenta' }).count() === 5 && await panel.locator('#account-setup-steps .badge', { hasText: 'Este computador' }).count() === 5);
  check(`${label}: the next action is announced and translated`, await panel.locator('#account-setup-next').innerText().then(text => text.includes('Faltan algunas variables')) && await panel.locator('#account-setup-state').innerText() === 'Requiere atención');
  const variables = rows.nth(4);
  check(`${label}: expected variables show present, waiting and missing without any value`, await variables.innerText().then(text => text.includes('DEPLOY_ENDPOINT: guardada, versión 3') && text.includes('PARTNER_TOKEN: esperando que escribas el valor') && text.includes('SIGNING_KEY: aún no está guardada')));
  const teams = await rows.nth(8).innerText();
  check(`${label}: Teams keeps signed in, running, received and sent apart`, teams.includes('Sesión de Microsoft iniciada.') && teams.includes('El asistente está detenido.') && teams.includes('4 recibidos · 1 enviados.') && teams.includes('Último mensaje recibido'));
  check(`${label}: versions show running, installed and available separately`, await rows.nth(9).innerText().then(text => text.includes('En ejecución 0.9.13 · instalada 0.9.13 · disponible 0.9.14.')));
  check(`${label}: finished steps have no button and open steps lead to their settings`, await rows.nth(0).locator('button').count() === 0 && await rows.nth(4).locator('button').count() === 1);
  check(`${label}: every row is text, never only color`, await panel.locator('#account-setup-steps .badge', { hasText: 'Pendiente' }).count() === 2);
  await rows.nth(4).locator('button').click();
  check(`${label}: a step opens the settings panel that fixes it`, await page.locator('#panel-variables').isVisible());
  await page.evaluate(() => showTab('overview'));
  check(`${label}: the panel keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: the readiness panel passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  // Becoming ready folds the guidance into a single confirmation.
  await page.evaluate(() => { window.__ptaReadiness = { ...window.__ptaReadiness, ready: true, next: null }; return loadReadiness(); });
  check(`${label}: a ready installation says so and shows no next action`, await panel.locator('#account-setup-state').innerText() === 'Lista' && await panel.locator('#account-setup-next').isHidden());
  // Leaving the account forgets the report.
  await page.evaluate(() => { leaveAccount(); });
  check(`${label}: leaving the account clears the report`, await panel.isHidden());
  check(`${label}: has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

// Account configuration: a signed-in installation shows what it loaded and what is still local setup.
for (const [label, ctx] of [['account-config-desktop', {}], ['account-config-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.evaluate(() => showTab('settings', 'panel-account-config'));
  check(`${label}: a coordinator or standalone installation shows no panel`, await page.locator('#panel-account-config').isHidden());
  await page.evaluate(() => {
    window.__ptaAccountConfig = { mode: 'replica', state: 'stale', coordinator: 'https://assistant.example.test', loaded: { generation: 4, digest: 'abc' }, saved: { generation: 5, digest: 'abd' },
      synced_at: Math.floor(Date.now() / 1000) - 300, error: null, conflict: null, local_changes: false,
      unavailable: { sources: [{ id: 'handbook-docs', repository: 'handbook' }], questions: ['where-is-the-guide'], partial: [] },
      local_setup: [{ id: 'repository:handbook', message: 'Connect the repository «handbook» on this computer; the sources that read it are unavailable until then.' },
        { id: 'roots', message: 'No directory is selected for coding agents on this computer; registering it does not grant access.' }] };
    window.__ptaReadiness = { ready: true, mode: 'signed_in', client: 'window', local_owner: true, coordinating: false, coordinator: 'https://assistant.example.test', next: null,
      steps: [{ id: 'coordinator', scope: 'coordinator', required: true, state: 'done', detail: "This computer is signed in to your account's coordinator.", facts: {}, actions: [] }],
      agent: { skill_version: '0.9.13' } };
    return Promise.all([loadAccountConfig(), loadReadiness()]);
  });
  await page.locator('#panel-account-config').waitFor();
  check(`${label}: loaded and account revisions are both shown`, await page.locator('#account-config-summary').innerText().then(text => text.includes('Revisión cargada 4') && text.includes('revisión de la cuenta 5')));
  check(`${label}: local setup is separated from what was restored and translated`, await page.locator('#account-config-setup').innerText().then(text => text.includes('Aún se necesita en este computador') && text.includes('Conecta el repositorio «handbook»') && text.includes('2 elementos de tu cuenta no están disponibles')));
  check(`${label}: the state is a text badge`, await page.locator('#account-config-state').innerText() === 'Hay una versión más nueva');
  check(`${label}: replacing local settings is hidden unless there is something to resolve`, await page.locator('#account-config-reset').isHidden());
  check(`${label}: the panel has no synchronize button of its own`, await page.locator('#panel-account-config button', { hasText: 'Sincronizar' }).count() === 0);
  await page.locator('#account-chip-action').click();
  await page.locator('#notice.ok').getByText('más recientes de tu cuenta').waitFor();
  check(`${label}: the sidebar's Synchronize reports the new state`, await page.locator('#account-config-state').innerText() === 'Sincronizado'
    && (await page.textContent('#account-chip-detail')).includes('Al día'));
  await page.evaluate(() => { window.__ptaAccountConfig = { ...window.__ptaAccountConfig, state: 'diverged', local_changes: true }; return loadAccountConfig(); });
  check(`${label}: unsaved local changes are explained and offer an explicit replacement`, await page.locator('#account-config-problem').innerText().then(text => text.includes('nunca se guardaron en la cuenta')) && await page.locator('#account-config-reset').isVisible());
  await page.locator('#account-config-reset').click();
  check(`${label}: replacing needs a second tap`, await page.locator('#account-config-reset').innerText() === 'Toca otra vez para reemplazar' && await page.evaluate(() => (window.__ptaAccountSyncs || []).every(args => !args.reset)));
  await page.locator('#account-config-reset').click();
  await page.locator('#notice.ok').getByText('copia privada').waitFor();
  check(`${label}: the second tap requests a reset`, await page.evaluate(() => window.__ptaAccountSyncs.at(-1).reset === true));
  await page.evaluate(() => { window.__ptaAccountFail = true; });
  await page.locator('#account-chip-action').click();
  await page.locator('#account-config-problem', { hasText: 'No se pudo contactar al coordinador' }).waitFor();
  check(`${label}: an outage is announced as status and keeps the page usable`, await page.locator('#account-config-problem').getAttribute('role') === 'status' && await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

// Account standing: offline and revoked installations say so; an account switch leaves nothing behind.
for (const [label, ctx] of [['standing-desktop', {}], ['standing-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.evaluate(() => {
    window.__ptaStanding = { state: 'live', account_key: 'aaaa0000aaaa0000', coordinator: 'https://assistant.example.test', last_live_at: Math.floor(Date.now() / 1000) - 60, recovery: '' };
    window.__ptaVariablesFor = { bbbb0000bbbb0000: [{ id: '00000000-0000-4000-8000-0000000000b1', name: 'BOB_ONLY', description: 'Belongs to the second account', version: 1, created_at: 1, updated_at: 1 }] };
    showTab('settings', 'panel-variables');
    return loadVariables();
  });
  await page.locator('#variables .row').first().waitFor();
  check(`${label}: a live installation shows no warning`, await page.locator('#variables-standing').isHidden());
  await page.evaluate(() => {
    window.__ptaStanding = { ...window.__ptaStanding, state: 'offline', recovery: 'The coordinator cannot be reached right now, so account variables cannot be used. The synchronized configuration stays readable but may be out of date; try again when this computer is online.' };
    return loadVariables();
  });
  await page.locator('#variables-standing').waitFor();
  check(`${label}: offline is announced in Spanish with the last confirmation`, await page.locator('#variables-standing').innerText().then(text => text.includes('Sin conexión') && text.includes('No se puede contactar al coordinador') && text.includes('Última confirmación')));
  check(`${label}: the cached list stays readable`, await page.locator('#variables .row').count() > 0);
  check(`${label}: the warning is a status region`, await page.locator('#variables-standing').getAttribute('role') === 'status');
  await page.evaluate(() => {
    window.__ptaStanding = { ...window.__ptaStanding, state: 'revoked', recovery: 'The coordinator no longer accepts this installation (it was signed out or revoked, or its account changed). Nothing was used. Sign in again from the sidebar or with `pta auth machine login`.' };
    return loadVariables();
  });
  check(`${label}: revoked is distinct and says how to sign in again`, await page.locator('#variables-standing').innerText().then(text => text.includes('Sesión cerrada o revocada') && text.includes('Vuelve a iniciar sesión')));
  await page.evaluate(axeSource);
  check(`${label}: passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  // Another account signs in: nothing typed or shown for the first one survives.
  await page.locator('#variable-name').fill('TYPED_FOR_ALICE');
  await page.locator('#variable-value').fill('synthetic-typed-value');
  const stale = await page.evaluate(() => accountEpoch);
  await page.evaluate(() => {
    window.__ptaStanding = { state: 'live', account_key: 'bbbb0000bbbb0000', coordinator: 'https://assistant.example.test', last_live_at: Math.floor(Date.now() / 1000), recovery: '' };
    return loadVariables();
  });
  await page.locator('#variables .row', { hasText: 'BOB_ONLY' }).waitFor();
  check(`${label}: the new account's list replaces the former one`, await page.locator('#variables').innerText().then(text => text.includes('BOB_ONLY') && !text.includes('DEPLOY') ));
  check(`${label}: typed values do not follow the switch and the warning is gone`, await page.locator('#variable-value').inputValue() === '' && await page.locator('#variables-standing').isHidden());
  check(`${label}: a late reply for the former account is refused`, await page.evaluate(old => adoptAccount(old, { account_key: 'aaaa0000aaaa0000' }), stale) === false
    && await page.locator('#variables').innerText().then(text => text.includes('BOB_ONLY')));
  check(`${label}: has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}

// Account variables: protected entry, metadata-only list and idempotent retries at both sizes.
for (const [label, ctx] of [['variables-desktop', {}], ['variables-phone', { viewport: { width: 393, height: 852 }, isMobile: true }]]) {
  const { context, page, errors } = await open({ axe: true, context: ctx });
  await page.evaluate(() => showTab('settings', 'panel-variables'));
  await page.locator('#variables .row').first().waitFor();
  check(`${label}: names and descriptions render as text`, await page.locator('#variables').innerText().then(text => text.includes('<b>not markup</b>')) && await page.locator('#variables b').count() === 0);
  check(`${label}: every variable shows a masked value and no plaintext`, await page.locator('#variables .variable-mask').count() === await page.locator('#variables .row').count()
    && await page.locator('#variables pre.variable-value').count() === 0
    && await page.locator('#variables').innerText().then(text => !text.includes('release.example.test') && !text.includes('SYNTHETIC')));
  check(`${label}: an agent request waits for a protected value`, await page.locator('#variables-pending').innerText().then(text => text.includes('PARTNER_ENDPOINT')));
  const mask = () => page.locator('#variable-value').evaluate(node => getComputedStyle(node).webkitTextSecurity);
  check(`${label}: the value field is masked by default`, await mask() === 'disc');
  await page.locator('#variable-show').check();
  check(`${label}: showing the value is an explicit choice`, await mask() === 'none');
  await page.locator('#variable-show').uncheck();
  check(`${label}: adding stays disabled until the form is complete`, await page.locator('#variable-create').isDisabled());

  const create = page.getByRole('button', { name: 'Agregar variable', exact: true });
  await page.fill('#variable-name', '1BAD');
  await page.fill('#variable-description', 'Synthetic description');
  await page.fill('#variable-value', 'synthetic-secret-value');
  await create.click();
  await page.locator('#notice.error').waitFor();
  check(`${label}: an invalid name is refused before the host`, await page.evaluate(() => window.__ptaVariableCreates.length === 0));
  await page.fill('#variable-name', 'NEW_VARIABLE');
  await page.evaluate(() => { window.__ptaVariableError = true; });
  await create.click();
  await page.waitForFunction(() => window.__ptaVariableCreates.length === 1);
  check(`${label}: a failed save keeps the typed value for a retry`, await page.inputValue('#variable-value') === 'synthetic-secret-value');
  await page.evaluate(() => { window.__ptaVariableError = false; });
  await create.click();
  await page.waitForFunction(() => window.__ptaVariableCreates.length === 2);
  check(`${label}: a retry reuses the entry ID`, await page.evaluate(() => window.__ptaVariableCreates[0].id === window.__ptaVariableCreates[1].id));
  await page.locator('#variables .row', { hasText: 'NEW_VARIABLE' }).waitFor();
  check(`${label}: the value field is cleared after storing`, await page.inputValue('#variable-value') === '' && await page.inputValue('#variable-name') === '');
  check(`${label}: nothing but metadata is rendered`, await page.locator('#panel-variables').innerText().then(text => !text.includes('synthetic-secret-value')));

  const pending = page.locator('#variables-pending');
  await pending.locator('textarea').fill('synthetic-partner-value');
  await pending.getByRole('button', { name: 'Guardar', exact: true }).click();
  await page.locator('#variables .row', { hasText: 'PARTNER_ENDPOINT' }).waitFor();
  check(`${label}: the agent's request completes through protected entry`, await pending.isHidden() && await page.evaluate(() => window.__ptaVariableCreates.at(-1).name === 'PARTNER_ENDPOINT'));
  check(`${label}: variables keep the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));

  // Everyday management: reveal, copy, edit each field, cancel, conflict and deletion.
  const row = name => page.locator('#variables .row', { hasText: name });
  const button = (name, text) => row(name).locator('button', { hasText: new RegExp(`^${text}$`) });
  const editor = name => row(name).locator('.variable-edit');
  const notice = () => page.locator('#notice').innerText();
  const clipboard = () => page.evaluate(() => navigator.clipboard.readText());
  const DEPLOY = 'DEPLOY_ENDPOINT', KEY = 'signing_key';
  const DEPLOY_ID = 'aaaaaaaa-0000-4000-8000-000000000001', KEY_ID = 'aaaaaaaa-0000-4000-8000-000000000002';
  const KEY_VALUE = '-----BEGIN SYNTHETIC KEY-----\nline two  with  spaces\n\tand a tab\n-----END SYNTHETIC KEY-----\n';

  check(`${label}: Variables replaces the separate credentials editor`, await page.locator('#settings-tab-variables').isVisible() && await page.locator('#panel-credentials').count() === 0);
  check(`${label}: Reveal has an eye icon and starts hidden`, await button(DEPLOY, 'Revelar').locator('use').getAttribute('href') === '#i-eye' && await button(DEPLOY, 'Revelar').getAttribute('aria-pressed') === 'false');
  await button(DEPLOY, 'Revelar').click();
  await row(DEPLOY).locator('pre.variable-value').waitFor();
  check(`${label}: Reveal fetches only the selected entry after the action`, await page.evaluate(id => window.__ptaVariableReveals.length === 1 && window.__ptaVariableReveals[0] === id, DEPLOY_ID));
  check(`${label}: the revealed eye control announces its pressed state`, await button(DEPLOY, 'Ocultar').getAttribute('aria-pressed') === 'true' && await button(DEPLOY, 'Ocultar').locator('use').getAttribute('href') === '#i-eye-off');
  check(`${label}: a revealed value is shown for that entry only`, await row(DEPLOY).locator('pre.variable-value').innerText() === 'https://release.example.test/v1' && await page.locator('#variables pre.variable-value').count() === 1);
  await button(DEPLOY, 'Ocultar').click();
  check(`${label}: Hide masks the value again`, await page.locator('#variables pre.variable-value').count() === 0 && await row(DEPLOY).locator('.variable-mask').count() === 1);
  await button(DEPLOY, 'Revelar').click();
  await row(DEPLOY).locator('pre.variable-value').waitFor();
  await page.evaluate(() => showTab('overview'));
  await page.evaluate(() => showTab('settings', 'panel-variables'));
  check(`${label}: navigating away clears a revealed value`, await page.locator('#variables pre.variable-value').count() === 0);
  await button(DEPLOY, 'Revelar').click();
  await row(DEPLOY).locator('pre.variable-value').waitFor();
  await page.evaluate(() => window.dispatchEvent(new Event('pagehide')));
  check(`${label}: leaving the page clears a revealed value`, await page.locator('#variables pre.variable-value').count() === 0);
  check(`${label}: a revealed value never reaches storage or the address`, await page.evaluate(() => !JSON.stringify([localStorage, sessionStorage, location.href]).includes('release.example.test')));

  // Copy writes the exact value (line breaks, tabs and the final newline) and shows no value anywhere.
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.clock.install();
  await button(KEY, 'Copiar valor').click();
  await page.locator('#notice', { hasText: 'copiado' }).waitFor();
  check(`${label}: Copy value writes the exact multiline value`, await clipboard() === KEY_VALUE);
  check(`${label}: copying shows no value in a notice or on the page`, await notice().then(text => !text.includes('SYNTHETIC')) && await page.locator('#panel-variables').innerText().then(text => !text.includes('SYNTHETIC')));
  await page.clock.fastForward(61_000);
  await page.waitForFunction(() => navigator.clipboard.readText().then(text => text === ''));
  check(`${label}: the clipboard is cleared when it still holds the copied value`, await clipboard() === '');
  await button(KEY, 'Copiar valor').click();
  await page.waitForFunction(() => window.__ptaVariableReveals.length === 5);
  await page.evaluate(() => navigator.clipboard.writeText('unrelated clipboard text'));
  await page.clock.fastForward(61_000);
  await page.waitForTimeout(300);
  check(`${label}: cleanup never overwrites unrelated clipboard contents`, await clipboard() === 'unrelated clipboard text');
  check(`${label}: Copy fetched the value only on demand`, await page.evaluate(() => window.__ptaVariableReveals.length === 5));
  await button(DEPLOY, 'Revelar').click();
  await row(DEPLOY).locator('pre.variable-value').waitFor();
  await page.clock.fastForward(31_000);
  check(`${label}: a revealed value hides itself after a short time`, await page.locator('#variables pre.variable-value').count() === 0);

  // A description edit is independent of the value, and Cancel discards everything typed.
  await button(DEPLOY, 'Editar').click();
  check(`${label}: saving a description needs a change`, await editor(DEPLOY).getByRole('button', { name: 'Guardar descripción' }).isDisabled());
  check(`${label}: the replacement value field is masked`, await editor(DEPLOY).locator('textarea').evaluate(node => getComputedStyle(node).webkitTextSecurity) === 'disc');
  await editor(DEPLOY).getByRole('textbox', { name: /^Descripción de / }).fill('Discarded description');
  await editor(DEPLOY).locator('textarea').fill('discarded-value');
  const updatesBefore = await page.evaluate(() => window.__ptaVariableUpdates.length);
  await editor(DEPLOY).getByRole('button', { name: 'Cancelar', exact: true }).click();
  await button(DEPLOY, 'Editar').click();
  check(`${label}: Cancel discards edits without calling the host`, await page.evaluate(n => window.__ptaVariableUpdates.length === n, updatesBefore)
    && await editor(DEPLOY).getByRole('textbox', { name: /^Descripción de / }).inputValue() === 'Base URL of the release service <b>not markup</b>' && await editor(DEPLOY).locator('textarea').inputValue() === '');
  await editor(DEPLOY).getByRole('textbox', { name: /^Descripción de / }).fill('Release service base URL');
  await editor(DEPLOY).getByRole('button', { name: 'Guardar descripción' }).click();
  await row(DEPLOY).getByText('Release service base URL', { exact: true }).waitFor();
  check(`${label}: a description edit sends only the description and the seen version`, await page.evaluate(id => {
    const last = window.__ptaVariableUpdates.at(-1);
    return last.id === id && last.version === 1 && last.description === 'Release service base URL' && last.valueLength === undefined;
  }, DEPLOY_ID));
  check(`${label}: the edit advanced the version and closed the form`, await row(DEPLOY).innerText().then(text => text.includes('versión 2')) && await editor(DEPLOY).count() === 0);

  // Rotation changes only the value and keeps the entry and its description.
  await button(DEPLOY, 'Editar').click();
  await editor(DEPLOY).locator('textarea').fill('rotated-synthetic\nsecond line');
  await editor(DEPLOY).getByRole('button', { name: 'Reemplazar valor' }).click();
  await row(DEPLOY).getByText('versión 3').waitFor();
  check(`${label}: a rotation sends only the value and keeps the description`, await page.evaluate(id => {
    const last = window.__ptaVariableUpdates.at(-1);
    return last.id === id && last.version === 2 && last.description === undefined && last.valueLength === 'rotated-synthetic\nsecond line'.length;
  }, DEPLOY_ID) && await row(DEPLOY).innerText().then(text => text.includes('Release service base URL')));
  check(`${label}: the replaced value is not left on the page`, await page.locator('#panel-variables').innerText().then(text => !text.includes('rotated-synthetic')) && await page.locator('#panel-variables textarea').evaluateAll(fields => fields.every(field => !field.value.includes('rotated'))));
  await button(DEPLOY, 'Revelar').click();
  await row(DEPLOY).locator('pre.variable-value').waitFor();
  check(`${label}: Reveal returns the rotated value`, await row(DEPLOY).locator('pre.variable-value').innerText() === 'rotated-synthetic\nsecond line');
  await button(DEPLOY, 'Ocultar').click();

  // Another client's newer change is reported, never overwritten, and the typed text survives.
  await button(KEY, 'Editar').click();
  await editor(KEY).getByRole('textbox', { name: /^Descripción de / }).fill('My conflicting description');
  await page.evaluate(id => window.__ptaVariableChangedElsewhere(id), KEY_ID);
  await editor(KEY).getByRole('button', { name: 'Guardar descripción' }).click();
  await page.locator('#notice', { hasText: 'cambió desde que se cargó' }).waitFor();
  check(`${label}: a stale edit is reported as a conflict`, await notice().then(text => text.includes('cambió desde que se cargó')));
  await row(KEY).getByText('(edited elsewhere)').waitFor();
  check(`${label}: the conflict refreshed the list and kept the other client's change`, await row(KEY).innerText().then(text => text.includes('(edited elsewhere)') && text.includes('versión 3')));
  check(`${label}: the conflict kept the typed description for another try`, await editor(KEY).getByRole('textbox', { name: /^Descripción de / }).inputValue() === 'My conflicting description');
  await editor(KEY).getByRole('button', { name: 'Cancelar', exact: true }).click();

  // A rename changes only the name: no value is read or sent, and the entry keeps its ID.
  await button(KEY, 'Editar').click();
  check(`${label}: renaming needs a different name`, await editor(KEY).getByRole('button', { name: 'Renombrar' }).isDisabled());
  await editor(KEY).getByRole('textbox', { name: /^Nombre nuevo de / }).fill('SIGNING_KEY');
  const revealsBeforeRename = await page.evaluate(() => window.__ptaVariableReveals.length);
  await editor(KEY).getByRole('button', { name: 'Renombrar' }).click();
  await page.locator('#variables .row', { hasText: 'SIGNING_KEY' }).getByText('versión 4').waitFor();
  check(`${label}: a rename sends only the ID, the seen version and the new name`, await page.evaluate(([id, reveals]) => {
    const last = window.__ptaVariableRenames.at(-1);
    return last.id === id && last.version === 3 && last.name === 'SIGNING_KEY' && last.keys === 'id,name,version' && window.__ptaVariableReveals.length === reveals;
  }, [KEY_ID, revealsBeforeRename]));
  await page.locator('#notice', { hasText: 'se renombró a SIGNING_KEY' }).waitFor({ timeout: 5000 }).catch(() => {});
  check(`${label}: the rename is confirmed in Spanish and closes the form`, await notice().then(text => text.includes('se renombró a SIGNING_KEY')) && await editor(KEY).count() === 0, await notice());

  // Axe sees an open editor and a revealed value, then deletion asks twice.
  await button(DEPLOY, 'Editar').click();
  await button(KEY, 'Revelar').click();
  await row(KEY).locator('pre.variable-value').waitFor();
  await page.evaluate(axeSource);
  check(`${label}: variable actions and editor pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await editor(DEPLOY).getByRole('button', { name: 'Cancelar', exact: true }).click();
  await button(KEY, 'Ocultar').click();
  const deletesBefore = await page.evaluate(() => window.__ptaVariableDeletes.length);
  await button(KEY, 'Eliminar').click();
  check(`${label}: Delete asks for a second tap before calling the host`, await button(KEY, 'Confirmar eliminación').count() === 1 && await page.evaluate(n => window.__ptaVariableDeletes.length === n, deletesBefore)
    && await row(KEY).innerText().then(text => text.includes('No revoca la credencial')));
  await row(KEY).locator('button', { hasText: /^Cancelar$/ }).click();
  check(`${label}: cancelling the confirmation keeps the variable`, await button(KEY, 'Eliminar').count() === 1 && await page.evaluate(n => window.__ptaVariableDeletes.length === n, deletesBefore));
  await button(KEY, 'Eliminar').click();
  await button(KEY, 'Confirmar eliminación').click();
  await page.locator('#variables .row', { hasText: KEY }).waitFor({ state: 'detached' });
  check(`${label}: a confirmed deletion names the seen version and removes the entry`, await page.evaluate(([n, id]) => window.__ptaVariableDeletes.length === n + 1 && window.__ptaVariableDeletes.at(-1).id === id && window.__ptaVariableDeletes.at(-1).version === 4, [deletesBefore, KEY_ID]));
  const listsBefore = await page.evaluate(() => window.__ptaCalls.filter(call => call === 'variable_list').length);
  await page.locator('#variables-refresh').click();
  await page.locator('#notice.ok').getByText('Variables actualizadas').waitFor();
  check(`${label}: Refresh reloads the list on request`, await page.evaluate(n => window.__ptaCalls.filter(call => call === 'variable_list').length > n, listsBefore));
  // Consumers: pick a variable by name, test it and stop; only references and status move.
  const consumers = page.locator('#consumers .row').first();
  const consumerCalls = () => page.evaluate(() => window.__ptaConsumerCalls.map(call => call.method));
  check(`${label}: the consumer states its fixed destination and shows no value`, await consumers.innerText().then(text => text.includes('https://api.deepseek.com') && text.includes('Ninguna variable elegida') && !text.includes('release.example.test')));
  check(`${label}: choosing a variable is required before using it`, await consumers.getByRole('button', { name: 'Usar esta variable' }).isDisabled());
  await consumers.getByRole('combobox').selectOption(DEPLOY_ID);
  await consumers.getByRole('button', { name: 'Usar esta variable' }).click();
  await consumers.locator('.badge', { hasText: 'Usa DEPLOY_ENDPOINT' }).waitFor();
  check(`${label}: binding names the variable by ID and the seen version, then asks to restart`, await page.evaluate(id => {
    const call = window.__ptaConsumerCalls.at(-1);
    return call.method === 'bind' && call.variable === id && call.version === 3 && call.consumer === 'deepseek';
  }, DEPLOY_ID) && await consumers.innerText().then(text => text.includes('Reinicia el asistente')));
  await consumers.getByRole('button', { name: /^Probar clave/ }).click();
  await consumers.getByText('aceptó la clave').waitFor();
  check(`${label}: a test shows only a status sentence`, JSON.stringify(await consumerCalls()) === '["bind","check"]' && await consumers.innerText().then(text => !text.includes('rotated-synthetic') && !text.includes('release.example.test')));
  check(`${label}: a proposed launcher is listed as text with its command, folder and variables, waiting for approval`, await page.locator('#launchers').innerText().then(text => text.includes('release-tool') && text.includes('/usr/local/bin/release --fixed') && text.includes('/home/user/work') && text.includes('RELEASE_URL desde DEPLOY_ENDPOINT') && text.includes('<b>not markup</b>') && text.includes('Esperando tu aprobación')) && await page.locator('#launcher-list b').count() === 0);
  await page.evaluate(axeSource);
  check(`${label}: consumers pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: consumers keep the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  // The owner's portal reviews and approves a launcher; removing it stays on the machine.
  await page.evaluate(async () => { window.__ptaLaunchersHere = false; await loadConsumers(); });
  check(`${label}: in the portal a launcher can be approved but not removed`, await page.locator('#launcher-list').getByRole('button', { name: /^Eliminar/ }).count() === 0
    && await page.locator('#launcher-list').getByRole('button', { name: 'Aprobar el lanzador release-tool' }).count() === 1);
  await page.evaluate(async () => { window.__ptaLaunchersHere = true; await loadConsumers(); });
  await page.locator('#launcher-list').getByRole('button', { name: 'Aprobar el lanzador release-tool' }).click();
  await page.locator('#launcher-list .badge', { hasText: 'Aprobado' }).waitFor();
  check(`${label}: approving sends the digest of the definition shown and hides the button`, await page.evaluate(() => {
    const call = window.__ptaConsumerCalls.at(-1);
    return call.method === 'approve' && call.id === 'release-tool' && call.digest === 'd'.repeat(64);
  }) && await page.locator('#launcher-list').getByRole('button', { name: /^Aprobar/ }).count() === 0);
  await consumers.getByRole('button', { name: 'Dejar de usar' }).click();
  await consumers.locator('.badge', { hasText: 'Ninguna variable elegida' }).waitFor();
  await page.locator('#launcher-list').getByRole('button', { name: /^Eliminar/ }).click();
  await page.locator('#launchers').waitFor({ state: 'hidden' });
  check(`${label}: approving, stopping and removing call only their own operations`, JSON.stringify(await consumerCalls()) === '["bind","check","approve","unbind","remove"]');
  // Delivery destinations: a person approves exactly what is shown; a delivery asks twice and reports only a status.
  const deliveries = page.locator('#deliveries');
  check(`${label}: a proposed destination shows its address, headers, body, credential and variables as text`, await deliveries.innerText().then(text => text.includes('PUT https://editor.example.test/api/import/secrets/OPENAI_API_KEY')
    && text.includes('X-Blog-Editor: 1') && text.includes('Authorization: Bearer ‹DEPLOY_ENDPOINT›') && text.includes('{"value":"{{value}}"}') && text.includes('Puede recibir: DEPLOY_ENDPOINT')
    && text.includes('<b>not markup</b>') && text.includes('Esperando tu aprobación')) && await page.locator('#delivery-list b').count() === 0);
  check(`${label}: nothing can be delivered before approval`, await deliveries.getByRole('button', { name: /^Entregar/ }).count() === 0);
  await deliveries.getByRole('button', { name: 'Aprobar el destino blog-editor-openai' }).click();
  await deliveries.locator('.badge', { hasText: 'Aprobado' }).waitFor();
  await deliveries.getByRole('button', { name: 'Entregar DEPLOY_ENDPOINT' }).click();
  check(`${label}: a delivery asks for a second tap and says where the value goes`, await page.evaluate(() => window.__ptaDeliveryCalls.length === 1)
    && await deliveries.innerText().then(text => text.includes('lo rotes en su proveedor')));
  await page.evaluate(axeSource);
  check(`${label}: delivery destinations pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await deliveries.getByRole('button', { name: 'Confirmar la entrega de DEPLOY_ENDPOINT' }).click();
  await deliveries.getByText('Entregada a blog-editor-openai').waitFor();
  check(`${label}: approving and delivering send the shown digest and only the reference`, await page.evaluate(() => {
    const [approve, deliver] = window.__ptaDeliveryCalls;
    return approve.digest === 'e'.repeat(64) && deliver.destination === 'blog-editor-openai' && deliver.variable === 'aaaaaaaa-0000-4000-8000-000000000001'
      && Object.keys(deliver).sort().join(',') === 'destination,method,variable,version';
  }) && await deliveries.innerText().then(text => text.includes('HTTP 200') && text.includes('confirmó el valor')));
  await deliveries.getByRole('button', { name: 'Eliminar el destino blog-editor-openai' }).click();
  await deliveries.waitFor({ state: 'hidden' });
  // Migration: choose entries, write descriptions and confirm the account; no value is ever involved.
  await page.evaluate(() => showTab('settings', 'panel-migration'));
  const migration = page.locator('#panel-migration');
  await migration.getByText('Cuenta de destino').waitFor();
  check(`${label}: migration names the destination account, lists eligible rows and hides excluded ones`, await migration.innerText().then(text => text.includes('Account 1001 on https://portal.example.test') && text.includes('DEEPSEEK_API_KEY') && text.includes('Nunca se mueven')) && await migration.locator('#migration-excluded-list').isHidden());
  check(`${label}: nothing is copied before a choice and the account is confirmed`, await migration.locator('#migration-run').isDisabled());
  await migration.getByRole('checkbox', { name: 'Mover DEEPSEEK_API_KEY' }).check();
  check(`${label}: a chosen row asks for its description and offers the switch`, await migration.getByLabel('Descripción').count() === 1 && await migration.getByText('Cambiar DeepSeek API a la variable de la cuenta').count() === 1);
  check(`${label}: confirming the destination is still required`, await migration.locator('#migration-run').isDisabled());
  await migration.locator('#migration-confirm').check();
  check(`${label}: choosing, describing and confirming enables the copy`, await migration.locator('#migration-run').isEnabled());
  await page.evaluate(axeSource);
  check(`${label}: migration passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: migration keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await migration.locator('#migration-run').click();
  await migration.locator('#migration-result').getByText('Hecho').first().waitFor();
  check(`${label}: the copy names the destination key, the credential and the switch, never a value`, await page.evaluate(() => {
    const call = window.__ptaMigrationCalls.at(-1);
    return call.destination === 'dest-1001' && call.items.length === 1 && call.items[0].credential === 'DEEPSEEK_API_KEY'
      && call.items[0].bind === true && !JSON.stringify(call).includes('value') && !call.settings;
  }) && await migration.locator('#migration-result').innerText().then(text => text.includes('se conservaron como copias')));
  check(`${label}: variable management keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.evaluate(axeSource);
  check(`${label}: variables pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: variables have no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Updates: the owner is offered a prepared (not installed) update; a shared account only sees the managed notice.
for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport, ...(label === 'phone' ? { isMobile: true, hasTouch: true } : {}) } });
  const offer = page.locator('#update-offer');
  await offer.waitFor({ timeout: 5000 });
  check(`${label}: Home offers the newer release without installing it`, await offer.innerText().then(text => text.includes('0.9.14') && text.includes('0.9.13') && text.includes('sin cambiar nada')) && await page.evaluate(() => window.__ptaUpdateCalls.length === 0));
  await page.evaluate(axeSource);
  check(`${label}: the update notice passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.evaluate(() => showTab('settings', 'panel-updates'));
  const panel = page.locator('#panel-updates');
  await panel.waitFor();
  check(`${label}: the panel lists running, installed and available versions separately`, await panel.locator('#update-versions').innerText().then(text => /En ejecución\s*0\.9\.13/.test(text) && /Instalada\s*0\.9\.13/.test(text) && /Disponible\s*0\.9\.14/.test(text)));
  check(`${label}: preparing needs an explicit press`, await panel.locator('#update-stage').isEnabled() && await page.evaluate(() => window.__ptaUpdateCalls.length === 0));
  await panel.locator('#update-stage').click();
  await panel.locator('#update-progress').getByText('Compilando la versión 0.9.14').waitFor();
  check(`${label}: preparing asks only for the version the check offered`, await page.evaluate(() => JSON.stringify(window.__ptaUpdateCalls) === '[{"method":"stage","version":"0.9.14"}]'));
  check(`${label}: progress is shown and a second build cannot be started`, await panel.locator('#update-stage').isDisabled() && await panel.locator('#update-state').innerText() === 'Preparando');
  await page.evaluate(() => { window.__ptaUpdateFinish(); return loadUpdates(); });
  await panel.locator('#update-progress').getByText('Aún no está instalada').waitFor();
  check(`${label}: a prepared update is never reported as installed`, await panel.locator('#update-versions').innerText().then(text => /Instalada\s*0\.9\.13/.test(text)) && await panel.locator('#update-state').innerText() === 'Preparada');
  // Installing: one press arms, the second sends the exact prepared version; progress follows the host's journal.
  check(`${label}: a prepared update offers Install both on Home and in the panel`, await offer.locator('#update-offer-apply').isVisible() && await offer.locator('#update-offer-stage').isHidden() && await panel.locator('#update-apply').isVisible());
  await page.evaluate(axeSource);
  check(`${label}: the install notice passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await panel.locator('#update-apply').click();
  check(`${label}: the first press only asks to press again`, await panel.locator('#update-apply').innerText() === 'Toca de nuevo para instalar y reiniciar' && await page.evaluate(() => !window.__ptaUpdateCalls.some(c => c.method === 'apply')));
  await panel.locator('#update-apply').click();
  await panel.locator('#update-progress').getByText('Instalando la versión 0.9.14').waitFor();
  check(`${label}: installing asks only for the prepared version`, await page.evaluate(() => JSON.stringify(window.__ptaUpdateCalls.filter(c => c.method === 'apply')) === '[{"method":"apply","version":"0.9.14"}]'));
  check(`${label}: installing hides the other actions and is labelled`, await panel.locator('#update-state').innerText() === 'Instalando' && await panel.locator('#update-stage').isDisabled() && await panel.locator('#update-apply').isHidden() && await offer.isHidden());
  await page.evaluate(() => { window.__ptaUpdateSet('applying', { state: 'restarting', phase: 'restarting' }); return loadUpdates(); });
  await panel.locator('#update-progress').getByText('Reiniciando la aplicación').waitFor();
  await page.evaluate(() => { window.__ptaUpdateSet('applying', { state: 'verifying', phase: 'verifying' }); return loadUpdates(); });
  await panel.locator('#update-progress').getByText('Comprobando que la versión 0.9.14 funciona bien').waitFor();
  check(`${label}: the installed version is not shown as the new one until the host says success`, await panel.locator('#update-versions').innerText().then(text => /Instalada\s*0\.9\.13/.test(text)) && await panel.locator('#update-result').isHidden());
  await page.evaluate(() => { window.__ptaUpdateSet('idle', { state: 'success', phase: 'success', notes: ['Refreshed 1 installed skill copy to version 0.9.14.', 'Agents that keep `pta mcp` running still use the previous tool catalog: reconnect or restart their MCP server so they read version 0.9.14.'] }); return loadUpdates(); });
  await panel.locator('#update-result').waitFor();
  check(`${label}: success is reported from the journal with the reconnect instruction`, await panel.locator('#update-result').innerText().then(text => text.includes('La versión 0.9.14 está instalada y funciona bien') && text.includes('Se actualizó 1 copia instalada') && text.includes('reconecta o reinicia su servidor MCP')));
  await page.evaluate(() => { window.__ptaUpdateSet('staged', { state: 'rolled_back', phase: 'rolled_back', error: 'Version 0.9.14 did not become healthy in time.' }); return loadUpdates(); });
  check(`${label}: a rollback says the previous version runs again and why`, await panel.locator('#update-result').innerText().then(text => text.includes('La versión 0.9.14 no se conservó y la versión 0.9.13 funciona de nuevo') && text.includes('0.9.14')) && await panel.locator('#update-result').getAttribute('class').then(c => c.includes('warn')));
  await page.evaluate(() => { window.__ptaUpdateSet('idle', { state: 'failed', phase: 'failed', error: 'The previous version 0.9.13 does not start.', recovery: 'Reinstall the exact version with `cargo install personal-teams-assistant --version X.Y.Z --locked --force`, then start the application.' }); return loadUpdates(); });
  check(`${label}: a failed update gives the recovery path`, await panel.locator('#update-result-error').innerText().then(text => text.includes('cargo install')) && await panel.locator('#update-result-text').innerText().then(text => text.includes('necesita tu atención')));
  await page.evaluate(axeSource);
  check(`${label}: update outcomes pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  // Work in progress defers instead of interrupting; the wait can be cancelled.
  await page.evaluate(() => { window.__ptaUpdateSet('deferred', null); return loadUpdates(); });
  await panel.locator('#update-deferred').waitFor();
  check(`${label}: a deferred update lists the work it waits for and offers to cancel`, await panel.locator('#update-deferred').innerText().then(text => text.includes('Se está respondiendo o enviando un mensaje de Teams') && text.includes('Hay una migración en curso')) && await panel.locator('#update-cancel').isVisible() && await panel.locator('#update-apply').isHidden() && await panel.locator('#update-state').innerText() === 'Esperando a que termine el trabajo');
  await page.evaluate(axeSource);
  check(`${label}: the deferred update passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await panel.locator('#update-cancel').click();
  await panel.locator('#update-deferred').waitFor({ state: 'hidden' });
  check(`${label}: cancelling the wait goes back to a prepared update`, await panel.locator('#update-apply').isVisible() && await page.evaluate(() => window.__ptaUpdateCalls.at(-1).method === 'cancel'));
  await page.evaluate(() => { window.__ptaUpdateFail(); return loadUpdates(); });
  await panel.locator('#update-error').waitFor();
  check(`${label}: a failure says what to fix and keeps the build output on request`, await panel.locator('#update-error').innerText().then(text => text.includes('compilador y enlazador de C')) && await panel.locator('#update-tail summary').innerText() === 'Última salida de la compilación');
  await page.evaluate(axeSource);
  check(`${label}: the updates panel passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: the updates panel keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  // Unattended updating: off by default, this computer only, changed only by an explicit choice.
  const unattended = panel.locator('#update-unattended-toggle');
  check(`${label}: unattended updating is offered, off by default`, await unattended.isVisible() && !(await unattended.isChecked()) && await page.evaluate(() => !window.__ptaUpdateCalls.some(c => c.method === 'unattended')));
  check(`${label}: its choice is labelled in Spanish`, await panel.locator('#update-unattended').innerText().then(text => text.includes('Instalar las nuevas versiones solas en este computador') && text.includes('solo para este computador')));
  await unattended.check();
  await page.waitForFunction(() => window.__ptaUpdateCalls.some(c => c.method === 'unattended'));
  check(`${label}: turning it on sends only the choice`, await page.evaluate(() => JSON.stringify(window.__ptaUpdateCalls.filter(c => c.method === 'unattended')) === '[{"method":"unattended","enabled":true}]') && await unattended.isChecked());
  await page.evaluate(axeSource);
  check(`${label}: the unattended choice passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await panel.locator('#update-dismiss').click();
  await offer.waitFor({ state: 'hidden' });
  check(`${label}: dismissing hides the notice for that release`, await page.evaluate(() => window.__ptaUpdateCalls.at(-1).method === 'dismiss' && window.__ptaUpdateCalls.at(-1).version === '0.9.14'));
  check(`${label}: updates have no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
{
  const { context, page } = await open({ axe: true }, 'account');
  await page.evaluate(() => showTab('settings', 'panel-updates'));
  const panel = page.locator('#panel-updates');
  await panel.waitFor();
  await panel.locator('#update-managed', { hasText: 'operador' }).waitFor({ timeout: 5000 }).catch(() => {});
  check('shared account sees the managed-server notice instead of an install button', await panel.locator('#update-managed').innerText().then(text => text.includes('operador')) && await panel.locator('#update-stage').isHidden() && await panel.locator('#update-check').isHidden() && await page.locator('#update-offer').isHidden());
  check('shared account cannot choose unattended updating', await panel.locator('#update-unattended').isHidden());
  await page.evaluate(axeSource);
  check('shared account updates panel passes axe', (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await context.close();
}
// Cloud storage: the migration starts only on a second tap, then offers verify, promotion and
// rollback; after a rollback the copy can be erased, again on a second tap.
for (const [label, options] of [['desktop', {}], ['phone', { context: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true } }]]) {
  const { context, page, errors } = await open({ axe: true, ...options });
  await page.evaluate(() => { window.__ptaCloudToken = true; showTab('settings', 'panel-cloud'); });
  const panel = page.locator('#panel-cloud');
  await panel.locator('#cloud-migrate').waitFor();
  check(`${label}: maintenance is folded under the next step`, !(await panel.locator('#cloud-disconnect').isVisible()) && await panel.locator('#cloud-maintenance > summary').isVisible());
  await panel.locator('#cloud-maintenance > summary').click();
  await panel.locator('#cloud-mirror').waitFor();
  const start = panel.locator('#cloud-migrate');
  const rollback = panel.locator('#cloud-rollback');
  check(`${label}: the migration is offered with the key notice`, await start.isVisible() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('pta variables key backup') && await panel.locator('#cloud-erase-box').isHidden());
  check(`${label}: a connected computer does not offer to connect again`, await panel.locator('#cloud-connect-fold').isHidden());
  await start.click();
  check(`${label}: the first tap only arms the migration`, (await start.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudMirror));
  // A long action stays armed longer than a quick double tap would need.
  await page.waitForTimeout(5000);
  check(`${label}: the migration stays armed for a deliberate second tap`, (await start.innerText()).includes('Toca de nuevo'));
  await start.click();
  await rollback.waitFor();
  check(`${label}: the second tap migrates and offers verify and rollback`, await start.isHidden() && await panel.locator('#cloud-verify').isVisible() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('al día') && (await panel.locator('#cloud-mirror-summary').innerText()).includes('Último respaldo'));
  await panel.locator('#cloud-verify').click();
  await panel.locator('#cloud-verify-result').waitFor();
  check(`${label}: a verification lists changes not copied yet`, (await panel.locator('#cloud-verify-result').innerText()).includes('Mensajes'));
  const repairCopy = panel.locator('#cloud-repair');
  check(`${label}: a matching copy offers no repair`, await repairCopy.isHidden());
  await page.evaluate(async () => { window.__ptaCloudMirror.last_verify = { at: Math.floor(Date.now() / 1000), ok: false }; await loadCloud(); });
  check(`${label}: a copy that differs offers a targeted repair`, await repairCopy.isVisible() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('la copia difiere'));
  await repairCopy.click();
  check(`${label}: the first tap only arms the repair`, (await repairCopy.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudRepaired));
  await repairCopy.click();
  await repairCopy.waitFor({ state: 'hidden' });
  check(`${label}: the second tap repairs the copy and says it matches again`, await page.evaluate(() => window.__ptaCloudRepaired === true)
    && await page.waitForFunction(() => (document.querySelector('#notice-text')?.textContent ?? '').includes('vuelve a coincidir'), null, { timeout: 5000 }).then(() => true, () => false));
  const rotateKey = panel.locator('#cloud-rotate');
  await rotateKey.click();
  check(`${label}: the first tap only arms the key rotation`, (await rotateKey.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudRotated));
  await rotateKey.click();
  await page.waitForFunction(() => window.__ptaCloudRotated === true);
  check(`${label}: the second tap rotates the key and says to back it up`, await page.waitForFunction(() => {
    const text = document.querySelector('#notice-text')?.textContent ?? '';
    return text.includes('42') && text.includes('pta variables key backup');
  }, null, { timeout: 5000 }).then(() => true, () => false), await page.locator('#notice-text').textContent());
  const promote = panel.locator('#cloud-promote');
  check(`${label}: a verified copy can become the source`, await promote.isVisible() && await panel.locator('#cloud-demote').isHidden());
  await page.evaluate(() => { window.__ptaCloudConflicts = 1; });
  await promote.click();
  check(`${label}: the first tap only arms the promotion`, (await promote.innerText()).includes('Toca de nuevo') && await page.evaluate(() => window.__ptaCloudMirror.mode === 'mirror'));
  await promote.click();
  await panel.locator('#cloud-demote').waitFor();
  const conflicts = panel.locator('#cloud-conflicts');
  await conflicts.waitFor();
  check(`${label}: primary mode explains read-only and lists the conflict's fields as text`, await promote.isHidden() && await rollback.isVisible() && await panel.locator('#cloud-rollback-local').isVisible()
    && (await panel.locator('#cloud-mirror-summary').innerText()).includes('fuente')
    && await conflicts.innerText().then(text => text.includes('La configuración de la cuenta') && text.includes('policy.greeting') && text.includes('Hola <b>equipo</b>') && text.includes('Buenos días') && text.includes('(no está)') && text.includes('…'))
    && await conflicts.locator('b').count() === 0);
  await page.evaluate(axeSource);
  check(`${label}: primary mode passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  const keepStore = conflicts.getByRole('button', { name: /Conservar el del almacén de cuenta/ });
  await keepStore.click();
  check(`${label}: the first tap only arms the choice`, (await keepStore.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudResolved));
  await keepStore.click();
  await conflicts.waitFor({ state: 'hidden' });
  check(`${label}: the second tap keeps the store's version of that record and clears the conflict`, await page.evaluate(() => window.__ptaCloudResolved?.collection === 'settings' && window.__ptaCloudResolved?.record === 'config' && window.__ptaCloudResolved?.keep === 'store' && window.__ptaCloudMirror.primary.conflicts === 0));
  check(`${label}: without a conflict the store's version is not offered`, await panel.locator('#cloud-rollback-local').isHidden());
  await panel.locator('#cloud-demote').click();
  await panel.locator('#cloud-promote').waitFor();
  check(`${label}: returning to the copy offers the rollback again`, await page.evaluate(() => window.__ptaCloudMirror.mode === 'mirror') && await panel.locator('#cloud-demote').isHidden() && await rollback.isVisible());
  await page.evaluate(axeSource);
  check(`${label}: the cloud storage panel passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: the cloud storage panel keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  check(`${label}: an expiring service token is announced`, (await panel.locator('#cloud-details').innerText()).includes('renueve'));
  check(`${label}: the plan's shared quota is shown and an oversubscribed plan warns`, await panel.locator('#cloud-details').innerText().then(text => text.includes('compartidos por 6 cuentas') && text.includes('reduzca las partes')));
  const computers = panel.locator('#cloud-installation-list');
  check(`${label}: the account's computers are listed as text, this one and the revoked one without a revoke button`, await computers.innerText().then(text => text.includes('Synthetic <b>desktop</b>') && text.includes('Este computador') && text.includes('Acceso revocado'))
    && await computers.locator('b').count() === 0 && await computers.getByRole('button', { name: /^Revocar/ }).count() === 1);
  check(`${label}: the coordinator offers «Lost or stolen» for every other computer, revoked or not`, await computers.getByRole('button', { name: /^Perdido o robado/ }).count() === 2);
  const revokeOther = computers.getByRole('button', { name: 'Revocar Synthetic <b>desktop</b>' });
  await revokeOther.click();
  check(`${label}: the first tap only arms the revocation`, await page.evaluate(() => !window.__ptaCloudRevoked) && (await revokeOther.innerText()).includes('Toca de nuevo'));
  await revokeOther.click();
  await revokeOther.waitFor({ state: 'detached' });
  check(`${label}: the second tap revokes that computer by ID`, await page.evaluate(() => window.__ptaCloudRevoked === '00000000-0000-4000-8000-000000000002'));
  const lostButton = computers.getByRole('button', { name: 'Perdido o robado: Synthetic <b>desktop</b>' });
  await page.evaluate(() => { window.__ptaCloudLostPending = true; });
  await lostButton.click();
  check(`${label}: the first tap only arms securing the account`, await page.evaluate(() => !window.__ptaCloudLostArgs) && (await lostButton.innerText()).includes('Toca de nuevo'));
  await lostButton.click();
  const lostBox = panel.locator('#cloud-lost');
  await lostBox.waitFor();
  check(`${label}: a step that waits shows what to do, as text, and offers to continue`, await page.evaluate(() => window.__ptaCloudLostArgs.installation === '00000000-0000-4000-8000-000000000002')
    && await lostBox.innerText().then(text => text.includes('Synthetic <b>desktop</b>') && text.includes('Espera tu acción') && text.includes('Aprueba el launcher') && text.includes('Clave de contenido rotada'))
    && await lostBox.locator('b').count() === 0 && await panel.locator('#cloud-lost-continue').isVisible(), await lostBox.innerText());
  await page.evaluate(axeSource);
  check(`${label}: the lost computer steps pass axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await panel.locator('#cloud-lost-continue').click();
  await lostBox.waitFor({ state: 'hidden' });
  check(`${label}: continuing finishes the steps for the same computer and says to back up the key`, await page.evaluate(() => window.__ptaCloudLostArgs.installation === '00000000-0000-4000-8000-000000000002')
    && await page.waitForFunction(() => (document.querySelector('#notice-text')?.textContent ?? '').includes('pta variables key backup'), null, { timeout: 5000 }).then(() => true, () => false));
  const backupList = panel.locator('#cloud-backup-list');
  check(`${label}: the backups are listed and a damaged one cannot be restored`, await panel.locator('#cloud-backups').isVisible()
    && await backupList.innerText().then(text => text.includes('Antes de migrar') && text.includes('No se puede restaurar') && text.includes('no coincide'))
    && await backupList.getByRole('button').count() === 1);
  const restoreBackup = backupList.getByRole('button');
  await restoreBackup.click();
  check(`${label}: the first tap only arms the restore`, await page.evaluate(() => !window.__ptaCloudRestore) && (await restoreBackup.innerText()).includes('Toca de nuevo'));
  await restoreBackup.click();
  await page.waitForFunction(() => window.__ptaCloudRestore);
  check(`${label}: the second tap restores that backup by name and says the application restarts`, await page.evaluate(() => window.__ptaCloudRestore === 'cloud-1700000200')
    && await page.waitForFunction(() => (document.querySelector('#notice-text')?.textContent ?? '').includes('reiniciando'), null, { timeout: 5000 }).then(() => true, () => false));
  await page.evaluate(() => { window.__ptaCloudRestore = undefined; });
  const tokenBox = panel.locator('#cloud-token');
  const tokenList = panel.locator('#cloud-token-list');
  check(`${label}: the service token lists which computers wait for it, names as text`, await tokenBox.isVisible()
    && await tokenList.innerText().then(text => text.includes('Synthetic <i>desktop</i>') && text.includes('Esperando el token nuevo') && text.includes('Vuelve a confiarlo'))
    && await tokenList.locator('i').count() === 0 && (await panel.locator('#cloud-token-summary').innerText()).includes('2 computadores'));
  const expiryDate = panel.locator('#cloud-expiry-date');
  check(`${label}: the service token's expiry can be changed here and shows the recorded date`, await expiryDate.isVisible()
    && await expiryDate.inputValue() === await page.evaluate(() => new Date(cloudStatus.service_token.expires_at * 1000).toISOString().slice(0, 10)));
  await expiryDate.fill('2027-11-04');
  await panel.locator('#cloud-expiry-save').click();
  await page.waitForFunction(() => window.__ptaCloudConnectArgs && (document.querySelector('#notice-text')?.textContent ?? '').includes('fecha de vencimiento'));
  check(`${label}: saving the date connects again with only the endpoint and the date`, await page.evaluate(() => JSON.stringify(window.__ptaCloudConnectArgs) === JSON.stringify({ endpoint: 'https://store.example.com', token_expires: '2027-11-04' })));
  const replaceToken = panel.locator('#cloud-token-rotate');
  await replaceToken.click();
  check(`${label}: the first tap only arms the token replacement`, await page.evaluate(() => !window.__ptaLauncherRun) && (await replaceToken.innerText()).includes('Toca de nuevo'));
  await replaceToken.click();
  await page.waitForFunction(() => document.querySelector('#cloud-token-summary').textContent.includes('1 computador aún'));
  check(`${label}: the second tap runs the approved launcher and only the computer to trust again still waits`, await page.evaluate(() => window.__ptaLauncherRun === 'cloud-token-rotate'));
  check(`${label}: a computer with the new token but the earlier content key waits for the key`, (await tokenList.innerText()).includes('Esperando la clave de contenido nueva'));
  await page.screenshot({ path: `${out}/cloud-${label}.png`, fullPage: false });
  await rollback.click();
  check(`${label}: the first tap only arms the rollback`, (await rollback.innerText()).includes('Toca de nuevo') && await page.evaluate(() => window.__ptaCloudMirror.mode === 'mirror'));
  await rollback.click();
  const erase = panel.locator('#cloud-erase');
  await erase.waitFor();
  check(`${label}: after the rollback the copy stays and can be erased`, await rollback.isHidden() && await start.isVisible() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('hasta que la borres') && (await panel.locator('#cloud-erase-box').innerText()).includes('30 días'));
  await page.evaluate(axeSource);
  check(`${label}: the rolled-back panel passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await erase.click();
  check(`${label}: the first tap only arms the erasure`, (await erase.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudMirror.erased_at));
  await erase.click();
  await panel.locator('#cloud-erase-box').waitFor({ state: 'hidden' });
  check(`${label}: the second tap erases the copy and offers a new migration`, await start.isVisible() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('clave de contenido nueva'));
  const disconnect = panel.locator('#cloud-disconnect');
  await disconnect.click();
  check(`${label}: the first tap only arms the disconnection`, (await disconnect.innerText()).includes('Toca de nuevo') && await page.evaluate(() => !window.__ptaCloudDisconnected));
  await disconnect.click();
  await panel.locator('#cloud-mirror').waitFor({ state: 'hidden' });
  check(`${label}: the second tap disconnects and hides the copy`, await page.evaluate(() => window.__ptaCloudDisconnected === true));
  // A portal account: first without the operator's permission, then with it.
  await page.evaluate(async () => { window.__ptaCloudEnabled = false; await loadCloud(); });
  check(`${label}: a portal account the operator did not enable says so and offers no connection`, await panel.locator('#cloud-connect-fold').isHidden()
    && (await panel.locator('#cloud-problem').innerText()).includes('operador del portal'));
  await page.evaluate(async () => { window.__ptaCloudEnabled = true; window.__ptaCloudGranted = 'https://store.example.com'; await loadCloud(); });
  // The connection form is folded; what it offers is read from the elements themselves.
  check(`${label}: an enabled portal account connects to the operator's store without asking for a token`, await page.evaluate(() =>
    !document.querySelector('#cloud-connect-fold').hidden && document.querySelector('#cloud-client-id-field').hidden && document.querySelector('#cloud-client-secret-field').hidden && document.querySelector('#cloud-expires-field').hidden
    && !document.querySelector('#cloud-connect-granted').hidden && document.querySelector('#cloud-endpoint').value === 'https://store.example.com' && document.querySelector('#cloud-endpoint').readOnly));
  // A second computer of the account: it adopts the account's configuration instead of migrating its own.
  await page.evaluate(async () => {
    window.__ptaCloudGranted = undefined; window.__ptaCloudDisconnected = false; window.__ptaCloudAdopt = true;
    window.__ptaCloudMirror = { key: { present: true }, mode: 'off' };
    await loadCloud();
  });
  const adoptButton = panel.locator('#cloud-adopt');
  check(`${label}: a second computer is offered to adopt the account's configuration, not to migrate its own`, await adoptButton.isVisible()
    && await panel.locator('#cloud-migrate').isHidden() && (await panel.locator('#cloud-mirror-summary').innerText()).includes('otro computador'));
  await adoptButton.click();
  check(`${label}: the first tap only arms the adoption`, (await adoptButton.innerText()).includes('Toca de nuevo') && await page.evaluate(() => window.__ptaCloudMigrateArgs?.adopt !== true));
  await adoptButton.click();
  await page.waitForFunction(() => window.__ptaCloudMigrateArgs?.adopt === true);
  check(`${label}: the second tap migrates adopting the account's configuration`, await page.evaluate(() => window.__ptaCloudMigrateArgs.action === 'start'));
  check(`${label}: cloud storage has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Start at login: the owner registers and removes this computer's entry with explicit presses;
// a shared portal account never sees the control.
for (const [label, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport, ...(label === 'phone' ? { isMobile: true, hasTouch: true } : {}) } });
  await page.evaluate(() => showTab('settings', 'panel-service'));
  const panel = page.locator('#panel-service');
  await panel.waitFor();
  check(`${label}: start at login is off and offers to turn it on`, (await panel.locator('#service-state').innerText()) === 'Apagado'
    && await panel.locator('#service-install').isVisible() && await panel.locator('#service-uninstall').isHidden()
    && await page.evaluate(() => !window.__ptaServiceCalls.includes('install')));
  await panel.locator('#service-install').click();
  await page.waitForFunction(() => window.__ptaServiceCalls.includes('install'));
  await panel.locator('#service-uninstall').waitFor();
  check(`${label}: once installed it names the manager and the entry, and says when lingering is needed`, (await panel.locator('#service-state').innerText()) === 'Activado'
    && (await panel.locator('#service-rows').innerText()).includes('Unidad de usuario de systemd')
    && (await panel.locator('#service-notes').innerText()).includes('loginctl enable-linger')
    && await panel.locator('#service-install').isHidden());
  await page.evaluate(axeSource);
  check(`${label}: start at login passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`${label}: start at login keeps the phone boundary`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await panel.locator('#service-uninstall').click();
  await panel.locator('#service-install').waitFor();
  check(`${label}: removing it turns it off`, (await panel.locator('#service-state').innerText()) === 'Apagado' && await page.evaluate(() => window.__ptaServiceCalls.includes('uninstall')));
  check(`${label}: start at login has no browser errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
{
  const { context, page, errors } = await open({}, 'account');
  await page.evaluate(() => showTab('settings'));
  await page.waitForTimeout(300);
  check('portal account: no start-at-login control', await page.locator('#panel-service').isHidden());
  check('portal account: start at login has no browser errors', errors.length === 0, errors.join('; '));
  await context.close();
}
// Workflows: navigation, the workflow and block editors with their sheets, Settings › Workflows,
// Home's status row and axe scans, at desktop and phone widths (synthetic data).
if (flag !== '--axe-only') for (const [label, viewport] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const phone = label === 'phone';
  const { context, page, errors } = await open({ axe: true, context: { viewport, ...(phone ? { isMobile: true, hasTouch: true } : {}) } });
  await page.evaluate(axeSource);
  const calls = () => page.evaluate(() => window.__ptaWorkflowCalls);
  const sheet = 'dialog.destination-picker';
  if (phone) {
    check('workflows phone: the floating bar keeps five destinations', await page.locator('nav > button:not(.nav-settings):visible').count() === 5, String(await page.locator('nav > button:not(.nav-settings):visible').count()));
    check('workflows phone: Workflows waits under More', await page.locator('nav [data-tab="workflows"]').isHidden());
    await page.click('#nav-more');
    await page.click('nav [data-tab="workflows"]');
    check('workflows phone: More opens the section and closes', await page.isVisible('#workflows') && await page.getAttribute('#nav-more', 'aria-expanded') === 'false'
      && await page.getAttribute('#nav-more', 'aria-current') === 'page');
  } else {
    const order = await page.$$eval('nav button[data-tab]', buttons => buttons.map(b => b.dataset.tab));
    check('workflows desktop: the sidebar lists Workflows after Activities, eight items', order.indexOf('workflows') === order.indexOf('registration') + 1 && order.length === 8, order.join(' '));
    await page.click('nav [data-tab="workflows"]');
  }
  await page.waitForSelector('[data-workflow="payments-features"]');
  check(`workflows ${label}: the section opens with the first view`, await page.getAttribute('#workflows-tab-list', 'aria-selected') === 'true' && await page.isVisible('#workflow-rows'));
  check(`workflows ${label}: a row summarizes project, repositories, machine, agent and blocks`,
    /Payments.*payments-api, payments-web.*Preview server.*Claude Code.*2 bloques/.test(await page.textContent('[data-workflow="payments-features"] .resource-facts small')));
  // Every row's editor, open, passes axe.
  await page.evaluate(() => { for (const d of document.querySelectorAll('#workflow-rows details')) d.open = true; });
  await page.waitForTimeout(400);
  check(`workflows ${label}: the list with its editors passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  check(`workflows ${label}: no free-text directory field`, await page.locator('#workflows input:not([type=checkbox]):not([type=search])').evaluateAll(inputs => inputs.every(i => !/dir|path|checkout/i.test(i.name + i.id + i.placeholder))));

  // A new workflow: project, repositories, checkout on the machine, agent; saved and verified.
  await page.click('#workflow-new');
  const editor = page.locator('[data-workflow="new"] .workflow-editor');
  await editor.locator('input').first().fill('Receipts worker');
  check(`workflows ${label}: the ID follows the name`, await editor.locator('input').nth(1).inputValue() === 'receipts-worker');
  await editor.locator('textarea').first().fill('Bugs of the receipts worker.');
  const unchosen = await page.textContent('#workflow-project-new');
  await page.click('#workflow-project-new');
  await page.waitForSelector(sheet);
  check(`workflows ${label}: the project sheet lists recent projects, then all`, await page.locator(`${sheet} h3`).count() === 2);
  check(`workflows ${label}: the project sheet passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.fill(`${sheet} input[type=search]`, 'recei');
  check(`workflows ${label}: the project sheet filters by name`, await page.locator(`${sheet} .directory-picker-row`).count() === 1);
  await page.keyboard.press('Escape');
  const refocused = await page.waitForFunction(() => document.activeElement?.id === 'workflow-project-new' && !document.querySelector('dialog.destination-picker'), null, { timeout: 5000 }).then(() => true, () => false);
  check(`workflows ${label}: Escape keeps the choice and returns focus`, refocused && await page.textContent('#workflow-project-new') === unchosen,
    await page.evaluate(() => `${document.activeElement?.tagName}#${document.activeElement?.id} ${document.querySelectorAll('dialog').length} dialog(s)`));
  await page.click('#workflow-project-new');
  await page.fill(`${sheet} input[type=search]`, 'pay');
  await page.locator(`${sheet} .directory-picker-row button`).first().click();
  check(`workflows ${label}: choosing a project fills the field`, await page.textContent('#workflow-project-new') === 'Payments');
  await editor.locator('button', { hasText: 'Elegir repositorios' }).click();
  await page.waitForSelector(sheet);
  check(`workflows ${label}: the repository sheet is a searchable multi-select`, await page.locator(`${sheet} input[type=checkbox]`).count() === 3 && await page.isVisible(`${sheet} input[type=search]`));
  check(`workflows ${label}: the repository sheet passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.locator(`${sheet} .directory-picker-row`, { hasText: 'receipts-worker' }).locator('input').check();
  await page.locator(`${sheet} .directory-picker-footer button`).click();
  check(`workflows ${label}: the chosen repository waits for its checkout`, await editor.locator('[data-repository] small', { hasText: 'Sin checkout elegido' }).count() === 1);
  await editor.locator('[data-repository] button', { hasText: 'Elegir checkout' }).click();
  await page.waitForSelector('dialog.directory-picker:not(.destination-picker) .directory-picker-row');
  check(`workflows ${label}: the checkout opens the shared directory sheet on the machine`, await page.evaluate(() => window.__ptaCalls.includes('implementation_browse')));
  await page.locator('dialog.directory-picker .directory-picker-row button', { hasText: '/work' }).first().click();
  await page.locator('dialog.directory-picker .directory-picker-row', { hasText: '/work/application' }).locator('input').check();
  await page.locator('dialog.directory-picker .directory-picker-footer button').click();
  await editor.locator('button', { hasText: 'Crear workflow' }).click();
  await page.waitForSelector('[data-workflow="receipts-worker"]');
  const created = (await calls()).find(c => c.method === 'workflow_save' && c.workflow?.id === 'receipts-worker');
  check(`workflows ${label}: creating saves project, repository, checkout and agent`, created && created.workflow.project === 'Payments' && created.workflow.organization === 'https://dev.azure.com/contoso'
    && created.workflow.checkouts['a1a1a1a1-0000-4000-8000-000000000003'] === '/work/application' && created.workflow.agent.provider === 'codex' && created.workflow.agent.model && created.workflow.agent.effort,
    JSON.stringify(created));
  const saved = page.locator('[data-workflow="receipts-worker"]');
  check(`workflows ${label}: the checkout reads as verified`, await saved.locator('[data-repository] .badge.ok').count() === 1);

  // A checkout whose origin names another repository is refused next to it.
  await saved.locator('[data-repository] button', { hasText: 'Cambiar checkout' }).click();
  await page.waitForSelector('dialog.directory-picker:not(.destination-picker) .directory-picker-row');
  await page.locator('dialog.directory-picker .directory-picker-row button', { hasText: '/work' }).first().click();
  await page.locator('dialog.directory-picker .directory-picker-row', { hasText: '/work/documentation' }).locator('input').check();
  await page.locator('dialog.directory-picker .directory-picker-footer button').click();
  await saved.locator('button', { hasText: 'Guardar workflow' }).click();
  await page.waitForTimeout(300);
  check(`workflows ${label}: a mismatched checkout shows the host's reason next to it`, await saved.locator('[data-repository] .badge.error').count() === 1
    && /names another repository/.test(await saved.locator('.workflow-editor > .callout.error').textContent()));
  // Another machine that cannot answer: saved with a warning, the checkout not verified.
  await saved.locator('.workflow-editor select').first().selectOption('22222222-2222-4222-8222-222222222222');
  await saved.locator('[data-repository] button', { hasText: 'Elegir checkout' }).click();
  await page.waitForSelector('dialog.directory-picker:not(.destination-picker) .directory-picker-row');
  await page.locator('dialog.directory-picker .directory-picker-row button', { hasText: '/work' }).first().click();
  await page.locator('dialog.directory-picker .directory-picker-row', { hasText: '/work/application' }).locator('input').check();
  await page.locator('dialog.directory-picker .directory-picker-footer button').click();
  await saved.locator('button', { hasText: 'Guardar workflow' }).click();
  await page.waitForTimeout(400);
  check(`workflows ${label}: an unverifiable checkout is saved with the host's warning`, await saved.locator('[data-repository] .badge.warn').count() === 1
    && await saved.locator('.workflow-editor > .callout.warn').isVisible());

  // Reorder blocks, disable with the switch, remove with a second tap.
  const payments = page.locator('[data-workflow="payments-features"]');
  await payments.locator('[data-block="base-branch"] button[data-move="1"]').click();
  await payments.locator('button', { hasText: 'Guardar workflow' }).click();
  await page.waitForTimeout(300);
  const reordered = (await calls()).filter(c => c.method === 'workflow_save' && c.workflow?.id === 'payments-features').pop();
  check(`workflows ${label}: blocks are reordered and saved in order`, reordered?.workflow.blocks.join() === 'notify-release,base-branch', JSON.stringify(reordered?.workflow?.blocks));
  // A box that lifts a fixed rule of the coding agent is reviewed first, then saved confirmed.
  const savesBefore = (await calls()).filter(c => c.method === 'workflow_save' && c.workflow?.id === 'payments-features').length;
  await payments.locator('input[data-allow="merge"]').check();
  await payments.locator('button', { hasText: 'Guardar workflow' }).click();
  await page.waitForTimeout(300);
  check(`workflows ${label}: allowing an action shows what it authorizes before anything is sent`,
    await payments.locator('.workflow-allow-review .callout.warn', { hasText: 'Completar pull requests' }).isVisible()
    && (await calls()).filter(c => c.method === 'workflow_save' && c.workflow?.id === 'payments-features').length === savesBefore);
  await payments.locator('button', { hasText: 'Confirmar y guardar' }).click();
  await page.waitForTimeout(300);
  const allowed = (await calls()).filter(c => c.method === 'workflow_save' && c.workflow?.id === 'payments-features').pop();
  check(`workflows ${label}: an allowed action is saved with confirmation`, allowed?.confirmed === true && allowed.workflow.allow.join() === 'merge', JSON.stringify(allowed));
  await payments.locator('input.switch').click();
  await page.waitForTimeout(300);
  check(`workflows ${label}: the switch disables a workflow and nothing else`, (await calls()).some(c => c.method === 'workflow_save' && c.id === 'payments-features' && c.enabled === false && !c.workflow));
  await saved.locator('button', { hasText: 'Eliminar workflow' }).click();
  check(`workflows ${label}: removing asks for a second tap`, !(await calls()).some(c => c.method === 'workflow_remove') && await saved.locator('button', { hasText: 'Toca de nuevo para eliminar' }).isVisible());
  await saved.locator('button', { hasText: 'Toca de nuevo para eliminar' }).click();
  await page.waitForTimeout(300);
  check(`workflows ${label}: the workflow is removed with confirmation`, (await calls()).some(c => c.method === 'workflow_remove' && c.id === 'receipts-worker' && c.confirmed === true)
    && await page.locator('[data-workflow="receipts-worker"]').count() === 0);

  // Blocks: the library, a block in use cannot be removed, a notification confirms chat and text.
  await page.click('#workflows-tab-blocks');
  await page.evaluate(() => { for (const d of document.querySelectorAll('#block-rows details')) d.open = true; });
  await page.waitForTimeout(400);
  check(`blocks ${label}: rows say where each block is used`, /Payments features/.test(await page.textContent('[data-block="base-branch"] .resource-facts')));
  check(`blocks ${label}: the library with its editors passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  const before = (await calls()).length;
  await page.locator('[data-block="base-branch"] button', { hasText: 'Eliminar bloque' }).click();
  check(`blocks ${label}: removing a block in use is refused with its workflows`, (await calls()).length === before
    && /Payments features/.test(await page.locator('[data-block="base-branch"] .block-editor > .callout.error').textContent()));
  await page.click('#block-new');
  const block = page.locator('[data-block="new"] .block-editor');
  await block.locator('select').first().selectOption('teams_notification');
  await block.locator('input').first().fill('Tell the releases chat');
  await page.click('#block-chat-new');
  await page.waitForSelector(sheet);
  check(`blocks ${label}: the chat sheet lists the personal chat and the user's chats`, await page.locator(`${sheet} .directory-picker-row`).count() === 5);
  check(`blocks ${label}: the chat sheet passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.fill(`${sheet} input[type=search]`, 'releases');
  await page.locator(`${sheet} .directory-picker-row button`).first().click();
  await block.locator('fieldset label', { hasText: 'Marta Ruiz' }).locator('input').check();
  await block.locator('textarea').fill('Released: ');
  await block.locator('.placeholder-list button', { hasText: '{link}' }).click();
  await block.locator('button', { hasText: 'Revisar y guardar' }).click();
  check(`blocks ${label}: a notification shows its chat and text before saving`, !(await calls()).some(c => c.method === 'workflow_block_save' && c.block.id === 'tell-the-releases-chat')
    && /Releases.*Marta Ruiz/.test(await block.locator('.block-confirm').textContent()) && (await block.locator('.block-confirm pre').textContent()) === 'Released: {link}');
  check(`blocks ${label}: the confirmation passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await block.locator('button', { hasText: 'Confirmar y guardar' }).click();
  await page.waitForSelector('[data-block="tell-the-releases-chat"]');
  const notification = (await calls()).find(c => c.method === 'workflow_block_save' && c.block.id === 'tell-the-releases-chat');
  check(`blocks ${label}: the notification is saved confirmed with its chat, mentions and template`, notification?.confirmed === true && notification.block.chat.id === '19:rel@thread.v2'
    && notification.block.mentions.join() === 'Marta Ruiz' && notification.block.template === 'Released: {link}' && notification.block.stage === 'done' && !('text' in notification.block), JSON.stringify(notification));
  // A chat named by its many members is cut to the host's limit, and the block is accepted.
  await page.click('#block-new');
  await block.locator('select').first().selectOption('teams_notification');
  await block.locator('input').first().fill('Tell the crowd');
  await page.click('#block-chat-new');
  await page.waitForSelector(sheet);
  await page.fill(`${sheet} input[type=search]`, 'Synthetic member 01');
  await page.locator(`${sheet} .directory-picker-row button`).first().click();
  const crowdName = await page.textContent('#block-chat-new');
  check(`blocks ${label}: a long chat name is cut to at most 200 characters with an ellipsis`, [...crowdName].length <= 200 && [...crowdName].length > 190 && crowdName.endsWith('…'), String([...crowdName].length));
  await block.locator('textarea').fill('Done: {item}');
  await block.locator('button', { hasText: 'Revisar y guardar' }).click();
  await block.locator('button', { hasText: 'Confirmar y guardar' }).click();
  await page.waitForSelector('[data-block="tell-the-crowd"]');
  check(`blocks ${label}: the block with the cut name is accepted`, (await calls()).some(c => c.method === 'workflow_block_save' && c.block.id === 'tell-the-crowd' && [...c.block.chat.name].length <= 200 && c.block.chat.name.endsWith('…')));
  // A launcher block lists the coordinator's approved launchers, with program and digest.
  await page.evaluate(() => invoke('variable_launcher_approve', { id: 'release-tool', digest: 'd'.repeat(64) }));
  // Reading the section again (showTab reloads it) lists what changed.
  await page.evaluate(() => showTab('workflows', 'workflows-blocks'));
  await page.waitForTimeout(400);
  await page.click('#block-new');
  await block.locator('select').first().selectOption('launcher');
  await block.locator('input').first().fill('Release in test');
  await block.locator('select').nth(2).selectOption('release-tool');
  check(`blocks ${label}: a launcher shows its program and digest`, /\/usr\/local\/bin\/release --fixed/.test(await block.locator('.facts').textContent()) && /d{64}/.test(await block.locator('.facts').textContent()));
  await block.locator('button', { hasText: 'Revisar y guardar' }).click();
  await block.locator('button', { hasText: 'Confirmar y guardar' }).click();
  await page.waitForSelector('[data-block="release-in-test"]');
  check(`blocks ${label}: the launcher block is saved confirmed`, (await calls()).some(c => c.method === 'workflow_block_save' && c.block.id === 'release-in-test' && c.confirmed && c.block.launcher.digest === 'd'.repeat(64)));
  // Where launchers are not listed (another account's portal), the launcher editor says why.
  await page.evaluate(() => { window.__ptaLaunchersHidden = true; });
  // Reading the section again (showTab reloads it) lists what changed.
  await page.evaluate(() => showTab('workflows', 'workflows-blocks'));
  await page.waitForTimeout(400);
  await page.click('#block-new');
  check(`blocks ${label}: launcher blocks are offered only on an owner transport`, await block.locator('select').first().locator('option[value="launcher"]').evaluate(option => option.disabled)
    && /página local/.test(await block.textContent()));
  await page.evaluate(() => { window.__ptaLaunchersHidden = false; });
  await block.locator('button', { hasText: 'Descartar' }).click();

  // Settings › Workflows from the section's link: ready states, watcher, director.
  await page.click('#workflows-tab-list');
  await page.click('#workflows-settings');
  await page.waitForSelector('#workflow-ready-states [data-project="Payments"]');
  // The process's states arrive per project after the settings.
  await page.waitForFunction(() => document.querySelectorAll('#workflow-ready-states [data-project="Exports"] input').length === 4).catch(() => {});
  check(`workflow settings ${label}: the section's link opens the category`, await page.getAttribute('#settings-tab-workflows', 'aria-selected') === 'true' && await page.isVisible('#panel-workflow-ready'));
  check(`workflow settings ${label}: ready states are the process's discovered states`, await page.locator('#workflow-ready-states [data-project="Payments"] input').count() === 5);
  check(`workflow settings ${label}: each project says which director it gets and from where`,
    /Director: .+, de /.test(await page.locator('#workflow-ready-states [data-project="Payments"] .workflow-director-line').textContent() ?? ''));
  check(`workflow settings ${label}: the category passes axe`, (await scan(page)).length === 0, JSON.stringify(await scan(page)));
  await page.locator('#workflow-ready-states [data-project="Exports"] label', { hasText: 'Approved' }).locator('input').check();
  await page.fill('#workflow-interval', '10');
  await page.selectOption('#workflow-director-model', 'claude-opus-5-5');
  check(`workflow settings ${label}: edits show the save bar and flag the category`, await page.isVisible('#savebar') && await page.isVisible('#settings-tab-workflows .flag.edited'));
  await page.click('#workflow-director-prompt');
  check(`workflow settings ${label}: the director's prompt opens in Guides and instructions`, await page.isVisible('#agent-guides') && await page.locator('#agent-prompts [data-prompt="direct"] details').evaluate(d => d.open));
  await page.evaluate(() => showTab('settings', 'panel-workflow-ready'));
  await page.waitForTimeout(500);
  check(`workflow settings ${label}: unsaved edits survive leaving the category and coming back`, await page.inputValue('#workflow-interval') === '10'
    && await page.locator('#workflow-ready-states [data-project="Exports"] label', { hasText: 'Approved' }).locator('input').isChecked()
    && await page.inputValue('#workflow-director-model') === 'claude-opus-5-5' && !(await calls()).some(c => c.method === 'workflow_settings_update'));
  await page.fill('#workflow-concurrency', '9');
  await page.click('#save');
  await page.waitForTimeout(300);
  check(`workflow settings ${label}: values out of range are refused before sending`, await page.isVisible('#workflow-settings-problem') && !(await calls()).some(c => c.method === 'workflow_settings_update'));
  await page.fill('#workflow-concurrency', '3');
  await page.click('#save');
  await page.waitForTimeout(600);
  const update = (await calls()).find(c => c.method === 'workflow_settings_update');
  check(`workflow settings ${label}: the save bar saves only what changed: ready states, watcher and director`, update?.watch_interval_minutes === 10 && update.concurrency === 3
    && !('pull_request_wait_days' in update) && !('project_directors' in update) && JSON.stringify(update.ready_states) === '[{"organization":"https://dev.azure.com/contoso","project":"Exports","states":["Approved"]}]'
    && update.director.model === 'claude-opus-5-5' && update.director.machine === '11111111-1111-4111-8111-111111111111', JSON.stringify(update));
  check(`workflow settings ${label}: saving clears the save bar`, await page.isHidden('#savebar') && await page.isHidden('#settings-tab-workflows .flag.edited'));

  // Home links to the ready states while an enabled workflow that starts by itself has none.
  await page.evaluate(async () => {
    window.__ptaWorkflowCalls.length = 0;
    await invoke('workflow_settings_update', { ready_states: [{ project: 'Payments', states: [] }] });
    await invoke('workflow_save', { id: 'payments-features', enabled: true });
  });
  await page.evaluate(() => showTab('settings', 'panel-workflow-ready'));
  await page.waitForTimeout(400);
  await page.evaluate(async () => { await refreshLive(); showTab('overview'); });
  await page.waitForTimeout(300);
  const tile = page.locator('#tiles .status-row', { hasText: 'Workflows' });
  check(`workflows ${label}: Home warns about a missing ready state`, await tile.count() === 1 && /Payments/.test(await tile.textContent()));
  await tile.click();
  check(`workflows ${label}: Home's row opens Settings › Workflows`, await page.getAttribute('#settings-tab-workflows', 'aria-selected') === 'true');
  check(`workflows ${label}: no page scrolls sideways`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  check(`workflows ${label}: no client errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Home reads only the saved workflow settings: no machine catalog or process states.
if (flag !== '--axe-only') {
  const { context, page, errors } = await open();
  await page.waitForTimeout(1200);
  const homeCalls = await page.evaluate(() => ({ methods: window.__ptaCalls, workflow: window.__ptaWorkflowCalls }));
  check('workflows Home: reads the saved workflow settings once and nothing heavier', homeCalls.methods.filter(m => m === 'workflow_settings').length === 1
    && !homeCalls.workflow.some(c => c.method === 'workflow_settings' && c.project) && !homeCalls.methods.includes('implementation_catalog'), JSON.stringify(homeCalls.methods));
  check('workflows Home: no client errors', errors.length === 0, errors.join('; '));
  await context.close();
}
// A computer signed in to a coordinator reads workflows but changes them there.
if (flag !== '--axe-only') {
  const { context, page, errors } = await open();
  await page.evaluate(() => { window.__ptaWorkflowsElsewhere = true; showTab('workflows'); });
  await page.waitForSelector('[data-workflow="payments-features"]');
  check('workflows elsewhere: rows without editors and a notice naming the coordinator', await page.isVisible('#workflows-notice') && await page.locator('#workflow-rows details').count() === 0
    && await page.isHidden('#workflow-new') && await page.locator('#workflow-rows input.switch').count() === 0);
  check('workflows elsewhere: no client errors', errors.length === 0, errors.join('; '));
  await context.close();
}
// Workflows, automatic starts and the Implement entry: Home lists the runs waiting on the user
// once (with the item's link), Settings › Workflows shows the watcher and its last poll, and the
// Implement form's Workflow select fills and locks what a workflow supplies (a named workflow, or
// the director's choice shown before anything starts), at desktop and phone widths.
for (const [label, viewport] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport } });
  await page.evaluate(axeSource);
  await page.waitForSelector('#workflow-attention .status-row');
  check(`automatic starts ${label}: Home lists the runs waiting for you once, each with its item's link`, await page.locator('#workflow-attention .status-row').count() === 2
    && await page.locator('#workflow-attention .ref', { hasText: '#5003' }).count() === 1 && await page.locator('#tiles .status-row', { hasText: '5003' }).count() === 0
    && /Plan por revisar/.test(await page.locator('#workflow-attention').innerText()) && /Necesita un workflow/.test(await page.locator('#workflow-attention').innerText()));
  check(`automatic starts ${label}: Home does not scroll sideways`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  const home = await scan(page);
  check(`automatic starts ${label}: Home passes WCAG 2.2 AA`, home.length === 0, JSON.stringify(home));
  await page.screenshot({ path: `${out}/${label}-home-workflow-attention.png`, fullPage: true });
  await page.click('#workflow-attention-open');
  check(`automatic starts ${label}: the attention panel opens the Workflows section`, await page.isVisible('#workflows'));

  // The watcher in Settings › Workflows.
  await page.evaluate(() => { showTab('settings'); selectTab($('#settings-tabs'), 'workflows'); });
  await page.waitForSelector('#workflow-watcher-state p');
  const watching = await page.locator('#workflow-watcher-state').innerText();
  check(`automatic starts ${label}: Settings shows the interval and the last poll`, /revisa cada 5 minutos/.test(watching) && /Última revisión a las .*: 1 ejecución iniciada/.test(watching), watching);
  check(`automatic starts ${label}: Settings shows the items the last poll saw by category`, /HU 0, bugs 1, tareas 1/.test(watching), watching);
  await page.evaluate(() => { window.__ptaWatcher = { state: 'idle', reason: '', interval_minutes: 5, last_poll: null, next_poll_at: null }; });
  await page.click('#workflow-settings-refresh');
  await page.waitForFunction(() => /inactiva/.test(document.querySelector('#workflow-watcher-state')?.textContent || ''));
  await page.evaluate(() => { window.__ptaWatcher = { state: 'unavailable', reason: '', interval_minutes: 5, last_poll: null, next_poll_at: null }; });
  await page.click('#workflow-settings-refresh');
  await page.waitForFunction(() => /solo en tu coordinador/.test(document.querySelector('#workflow-watcher-state')?.textContent || ''));
  await page.evaluate(() => { window.__ptaWatcher = { state: 'waiting_for_director', reason: '', waiting_projects: ['Payments'], interval_minutes: 5, last_poll: null, next_poll_at: null }; });
  await page.click('#workflow-settings-refresh');
  await page.waitForFunction(() => /espera un director/.test(document.querySelector('#workflow-watcher-state')?.textContent || ''));
  const waitingText = await page.locator('#workflow-watcher-state').innerText();
  check(`automatic starts ${label}: Settings says the watcher waits for a director and for which projects`, /Payments/.test(waitingText), waitingText);
  await page.evaluate(async () => { await refreshLive(); showTab('overview'); });
  await page.waitForSelector('#tiles .status-row:has-text("Falta un director")');
  check(`automatic starts ${label}: Home warns once that a director is missing`, await page.locator('#tiles .status-row', { hasText: 'Falta un director' }).count() === 1);
  await page.evaluate(() => { delete window.__ptaWatcher; showTab('settings'); selectTab($('#settings-tabs'), 'workflows'); });
  await page.click('#workflow-settings-refresh');
  const settings = await scan(page);
  check(`automatic starts ${label}: Settings › Workflows passes WCAG 2.2 AA`, settings.length === 0, JSON.stringify(settings));
  check(`automatic starts ${label}: Settings does not scroll sideways`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  check(`automatic starts ${label}: no client errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
for (const [label, viewport] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const { context, page, errors } = await open({ axe: true, context: { viewport } });
  await page.evaluate(axeSource);
  const openForm = async () => {
    await page.click('nav [data-tab="todo"]');
    await page.locator('#todo-list button[aria-label="Implementar work item"]').first().click();
    await page.waitForSelector('#implementation-provider option', { state: 'attached' });
    await page.waitForFunction(() => !document.querySelector('#implementation-machine').disabled);
  };
  await openForm();
  const values = await page.locator('#implementation-workflow option').evaluateAll(options => options.map(o => o.value));
  check(`implement workflow ${label}: the select offers none, the director and the enabled workflows of the item's project`, JSON.stringify(values) === JSON.stringify(['none', 'director', 'payments-features']), JSON.stringify(values));
  check(`implement workflow ${label}: none leaves the form as it was`, await page.locator('#implementation-machine').isEnabled() && await page.locator('#implementation-context').isEnabled());

  // A named workflow fills and locks the machine, directories, agent, model and effort.
  await page.fill('#implementation-context', 'Keep this note.');
  await page.selectOption('#implementation-machine', '22222222-2222-4222-8222-222222222222');
  await page.waitForFunction(() => !document.querySelector('#implementation-machine').disabled);
  await page.selectOption('#implementation-workflow', 'payments-features');
  await page.waitForFunction(() => !document.querySelector('#implementation-workflow-note').hidden);
  const locked = await page.evaluate(() => ({
    machine: [document.querySelector('#implementation-machine').value, document.querySelector('#implementation-machine').disabled],
    provider: [document.querySelector('#implementation-provider').value, document.querySelector('#implementation-provider').disabled],
    model: [document.querySelector('#implementation-model').value, document.querySelector('#implementation-model').disabled],
    effort: [document.querySelector('#implementation-effort').value, document.querySelector('#implementation-effort').disabled],
    context: document.querySelector('#implementation-context').disabled,
    directories: [...document.querySelectorAll('.implementation-paths code')].map(c => c.textContent),
    removable: document.querySelectorAll('.implementation-path button').length,
    note: document.querySelector('#implementation-workflow-note').textContent,
    start: document.querySelector('.implementation-footer button:last-child').disabled,
  }));
  check(`implement workflow ${label}: a workflow fills and locks machine, directories, agent, model and effort`, locked.machine[0] === '11111111-1111-4111-8111-111111111111' && locked.machine[1]
    && locked.provider[0] === 'claude' && locked.provider[1] && locked.model[0] === 'claude-opus-5-5' && locked.model[1] && locked.effort[0] === 'high' && locked.effort[1] && locked.context
    && JSON.stringify(locked.directories) === JSON.stringify(['/work/application', '/work/application/web']) && locked.removable === 0 && /Payments features/.test(locked.note) && !locked.start, JSON.stringify(locked));
  check(`implement workflow ${label}: the directory chooser stays closed while locked`, await page.locator('#implementation-content fieldset button.secondary', { hasText: 'Elegir directorios' }).isDisabled());
  const named = await scan(page);
  check(`implement workflow ${label}: the locked form passes WCAG 2.2 AA`, named.length === 0, JSON.stringify(named));
  await page.screenshot({ path: `${out}/${label}-implementation-workflow.png`, fullPage: true });
  // «No workflow» gives back what the person had chosen before the workflow took over.
  await page.selectOption('#implementation-workflow', 'none');
  await page.waitForFunction(() => !document.querySelector('#implementation-machine').disabled && document.querySelector('#implementation-context').value === 'Keep this note.');
  check(`implement workflow ${label}: no workflow restores the machine and the typed context`, await page.locator('#implementation-machine').inputValue() === '22222222-2222-4222-8222-222222222222'
    && await page.locator('#implementation-context').inputValue() === 'Keep this note.' && await page.locator('#implementation-context').isEnabled());
  await page.selectOption('#implementation-workflow', 'payments-features');
  await page.waitForFunction(() => document.querySelector('#implementation-machine').disabled);
  await page.locator('.implementation-footer button').last().click();
  await page.waitForSelector('#implementation-workflow-started');
  check(`implement workflow ${label}: Start stays disabled once the run started`, await page.locator('.implementation-footer button').last().isDisabled());
  const sent = await page.evaluate(() => window.__ptaLastImplementation);
  check(`implement workflow ${label}: Start sends the workflow and nothing the workflow supplies`, sent.workflow === 'payments-features' && !('machine' in sent) && !('directories' in sent) && !('provider' in sent) && !('context' in sent), JSON.stringify(sent));
  check(`implement workflow ${label}: the started run's state is shown inline with a link to Workflows`, /Planificando/.test(await page.locator('#implementation-workflow-started').innerText())
    && await page.locator('#implementation-workflow-started button', { hasText: 'Abrir la ejecución' }).count() === 1);
  await page.locator('#implementation-workflow-started button', { hasText: 'Refresh' }).or(page.locator('#implementation-workflow-started button', { hasText: 'Actualizar' })).first().click();
  await page.waitForFunction(() => /Plan por revisar/.test(document.querySelector('#implementation-workflow-started')?.textContent || ''));
  const started = await scan(page);
  check(`implement workflow ${label}: the started run passes WCAG 2.2 AA`, started.length === 0, JSON.stringify(started));
  await page.screenshot({ path: `${out}/${label}-implementation-workflow-started.png`, fullPage: true });
  check(`implement workflow ${label}: no sideways scroll`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  await page.locator('#implementation-workflow-started button', { hasText: 'Abrir la ejecución' }).click();
  await page.waitForSelector('#run-detail-content #run-timeline');
  check(`implement workflow ${label}: the link opens the run's detail`, await page.isVisible('#workflow-run-detail') && /Plan por revisar/.test(await page.locator('#run-detail-content').innerText()));

  // The director chooses: its choice and reason are shown before anything starts.
  await openForm();
  await page.selectOption('#implementation-workflow', 'director');
  await page.waitForSelector('#implementation-workflow-choice');
  const choice = await page.locator('#implementation-workflow-choice').innerText();
  const asked = await page.evaluate(() => window.__ptaLastImplementation);
  check(`implement workflow ${label}: the director's choice and reason are shown before any start`, asked.workflow === 'director' && /Payments features/.test(choice) && /checkout/.test(choice)
    && await page.locator('#implementation-workflow-started').count() === 0, choice);
  check(`implement workflow ${label}: the chosen workflow locks the form and Start waits for the person`, await page.locator('#implementation-machine').isDisabled()
    && await page.locator('.implementation-paths code').count() === 2 && await page.locator('.implementation-footer button').last().isEnabled());
  const chosen = await scan(page);
  check(`implement workflow ${label}: the director's choice passes WCAG 2.2 AA`, chosen.length === 0, JSON.stringify(chosen));
  await page.screenshot({ path: `${out}/${label}-implementation-workflow-director.png`, fullPage: true });
  await page.locator('.implementation-footer button').last().click();
  await page.waitForSelector('#implementation-workflow-started');
  check(`implement workflow ${label}: confirming starts the chosen workflow`, (await page.evaluate(() => window.__ptaLastImplementation)).workflow === 'payments-features');

  // A director that chooses none starts nothing and leaves the choice to the person.
  await page.evaluate(() => { window.__ptaDirectorNone = true; });
  await openForm();
  await page.selectOption('#implementation-workflow', 'director');
  await page.waitForSelector('#implementation-workflow-choice.warn');
  check(`implement workflow ${label}: no workflow chosen leaves Start disabled and the reason shown`, await page.locator('.implementation-footer button').last().isDisabled()
    && /Ningún workflow/.test(await page.locator('#implementation-workflow-choice').innerText()));
  await page.selectOption('#implementation-workflow', 'none');
  await page.waitForFunction(() => !document.querySelector('#implementation-machine').disabled);
  check(`implement workflow ${label}: none unlocks the form again`, await page.locator('#implementation-context').isEnabled() && await page.locator('#implementation-workflow-choice').count() === 0);
  await page.evaluate(() => { delete window.__ptaDirectorNone; });
  check(`implement workflow ${label}: no client errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
// Workflows › Runs: the list with the waiting runs first, a run's detail (timeline, children, sessions, pull
// requests, block outcomes), the plan-review gate, assign, resume and cancel with their second press, the
// director's summary and sending it to chats with mentions, Home's rows and the To-Do card link, with axe
// scans, at desktop and phone widths (synthetic data).
if (flag !== '--axe-only') for (const [label, viewport] of [['desktop', { width: 1280, height: 900 }], ['phone', { width: 393, height: 852 }]]) {
  const phone = label === 'phone';
  const { context, page, errors } = await open({ axe: true, context: { viewport, ...(phone ? { isMobile: true, hasTouch: true } : {}) } });
  await page.evaluate(axeSource);
  const calls = () => page.evaluate(() => window.__ptaWorkflowCalls);
  const detailText = () => page.locator('#run-detail-content').innerText();
  // The list: every run, those that wait for the person first. The Runs tab reached with the keyboard reads it.
  await page.evaluate(() => showTab('workflows'));
  await page.focus('#workflows-tab-list');
  await page.keyboard.press('End');
  await page.waitForSelector('#run-rows [data-run]');
  check(`runs ${label}: the Runs tab chosen with the keyboard reads its list`, await page.isVisible('#workflows-runs'));
  const order = await page.locator('#run-rows [data-run]').evaluateAll(rows => rows.map(r => r.dataset.state));
  check(`runs ${label}: the list shows every run with those waiting for the person first`, order.length === 5 && order.slice(0, 3).every(s => ['plan_review', 'unassigned', 'needs_attention'].includes(s)) && order.slice(3).every(s => ['coding', 'done'].includes(s)), order.join(' '));
  check(`runs ${label}: the summary counts the waiting runs`, /3 ejecuciones te esperan/.test(await page.locator('#runs-summary').innerText()));
  const listScan = await scan(page);
  check(`runs ${label}: the list passes WCAG 2.2 AA`, listScan.length === 0, JSON.stringify(listScan));
  await page.screenshot({ path: `${out}/${label}-workflow-runs.png`, fullPage: true });
  // Every row is a card with one layout: a state badge with text, then the number and title, the type and the facts,
  // each inside the card and none over another, with a short title and with one of 200 characters, in both modes.
  const cardsFit = () => page.locator('#run-rows [data-run]').evaluateAll(rows => rows.map(row => {
    const card = row.getBoundingClientRect();
    const parts = [...row.querySelector('.card-main').children].map(part => part.getBoundingClientRect());
    const inside = parts.every(r => r.left >= card.left && r.right <= card.right && r.top >= card.top && r.bottom <= card.bottom);
    const apart = parts.every((r, i) => i === 0 || r.top >= parts[i - 1].bottom - 0.5);
    const title = row.querySelector('.run-row-title');
    const order = [...row.querySelector('.card-main').children].map(part => part.className.split(' ')[0]);
    return { inside, apart, unclipped: title.scrollWidth <= title.clientWidth + 1, badge: Boolean(row.querySelector('.card-meta .badge')?.textContent.trim()), order: order.slice(0, 3).join() };
  }));
  const fitOk = fits => fits.length === 5 && fits.every(f => f.inside && f.apart && f.unclipped && f.badge && f.order === 'card-meta,run-row-title,run-row-kind');
  const shortFit = await cardsFit();
  check(`runs ${label}: every card shows its state badge, then the title, the type and the facts inside the card`, fitOk(shortFit), JSON.stringify(shortFit));
  await page.evaluate(() => window.__ptaRetitleRun('3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e01', 'Improve settlement reporting so that every merchant sees the daily, weekly and monthly sums of their settlements with the fees, the refunds and the chargebacks of each period, and can export'.padEnd(200, '.')));
  await page.click('#runs-refresh');
  await page.waitForFunction(() => (document.querySelector('#run-rows [data-run="3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e01"] .run-row-title')?.textContent.length ?? 0) > 200);
  const longFit = await cardsFit();
  check(`runs ${label}: a 200-character title wraps inside its card without covering the rest`, fitOk(longFit) && await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), JSON.stringify(longFit));
  await page.emulateMedia({ colorScheme: 'dark' });
  const darkScan = await scan(page);
  check(`runs ${label}: the list in dark mode passes WCAG 2.2 AA`, darkScan.length === 0, JSON.stringify(darkScan));
  await page.emulateMedia({ colorScheme: 'light' });
  await page.evaluate(() => window.__ptaRetitleRun('3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e01', 'Improve settlement reporting'));
  // Filters.
  await page.selectOption('#runs-state', 'done');
  await page.waitForFunction(() => document.querySelectorAll('#run-rows [data-run]').length === 1);
  check(`runs ${label}: the state filter narrows the list`, (await calls()).some(c => c.method === 'workflow_runs' && c.state === 'done'));
  // A run waiting in the line for a slot says so, with its place.
  await page.evaluate(() => window.__ptaQueueRun('3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e03', 2));
  await page.selectOption('#runs-state', 'coding');
  await page.waitForFunction(() => [...document.querySelectorAll('#run-rows [data-run]')].map(r => r.dataset.state).join() === 'coding');
  check(`runs ${label}: a run waiting for a slot shows its place in the line`, /Esperando un turno, posición 2/.test(await page.locator('#run-rows').innerText()));
  const queuedScan = await scan(page);
  check(`runs ${label}: the list with a run waiting for a slot passes WCAG 2.2 AA`, queuedScan.length === 0, JSON.stringify(queuedScan));
  await page.evaluate(() => window.__ptaQueueRun('3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e03', null));
  await page.selectOption('#runs-state', '');
  await page.waitForFunction(() => document.querySelectorAll('#run-rows [data-run]').length === 5);
  // The detail of the coding run: timeline, children with sessions and pull requests, block outcomes, a summary and its send.
  await page.click('#run-rows [data-run="3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e03"]');
  await page.waitForSelector('#run-detail-content #run-timeline');
  check(`runs ${label}: the detail shows the stages, the children, their pull requests and the block outcomes`,
    await page.locator('#run-timeline li').count() === 5 && await page.locator('#run-children [data-child]').count() === 2
    && await page.locator('#run-children .run-pulls li').count() === 1 && /Programando/.test(await detailText()) && await page.locator('#run-blocks .run-block').count() === 1);
  check(`runs ${label}: a child's agent session can be opened`, await page.locator('#run-children button', { hasText: 'Abrir sesión del agente' }).count() === 2);
  check(`runs ${label}: the stored summary and its send are shown`, await page.locator('#run-summary-text .md').count() === 1 && /1 envío/.test(await detailText()));
  const detailScan = await scan(page);
  check(`runs ${label}: the detail passes WCAG 2.2 AA`, detailScan.length === 0, JSON.stringify(detailScan));
  await page.screenshot({ path: `${out}/${label}-workflow-run-detail.png`, fullPage: true });
  check(`runs ${label}: no sideways scroll in the detail`, await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  // Summary and sending: the sheet, one confirmation with the exact text, mentions, outcomes per chat.
  await page.click('#run-summary');
  await page.waitForFunction(() => /1 resumen anterior/.test(document.querySelector('#run-detail-content')?.textContent || ''));
  check(`runs ${label}: Summary asks the director and shows the new text with the earlier one folded`, (await calls()).some(c => c.method === 'workflow_summary') && /va en «coding»/.test(await page.locator('#run-summary-text').innerText()));
  await page.click('#run-send');
  await page.waitForSelector('dialog.destination-picker');
  await page.locator('dialog.destination-picker .directory-picker-row', { hasText: 'Payments team' }).locator('input').check();
  await page.locator('dialog.destination-picker .directory-picker-row', { hasText: 'Ana Pérez' }).locator('input').check();
  await page.locator('dialog.destination-picker button', { hasText: 'Continuar' }).click();
  await page.waitForSelector('#run-send-panel');
  check(`runs ${label}: the send shows the exact text and each destination with its members`, await page.locator('#run-send-text').inputValue().then(v => /va en «coding»/.test(v))
    && await page.locator('#run-send-panel [data-chat]').count() === 2 && await page.locator('#run-send-panel [data-chat="19:abc@thread.v2"] input[type=checkbox]').count() === 3);
  await page.locator('#run-send-panel [data-chat="19:abc@thread.v2"] input[value="Victoria Gómez"]').check();
  await page.fill('#run-send-text', 'Resumen editado para el equipo.');
  const sendButton = page.locator('#run-send-panel button', { hasText: 'Confirmar y enviar' });
  await sendButton.click();
  check(`runs ${label}: nothing is sent before the second press`, !(await calls()).some(c => c.method === 'workflow_summary_send'));
  await page.locator('#run-send-panel button', { hasText: 'Enviar ahora a 2 chat(s)' }).click();
  await page.waitForFunction(() => document.querySelectorAll('#run-send-panel .send-outcome[data-done]').length === 2);
  const sends = (await calls()).filter(c => c.method === 'workflow_summary_send');
  check(`runs ${label}: one confirmed send per chat, with the mention and the edited text`, sends.length === 2 && sends.every(s => s.confirmed && s.text === 'Resumen editado para el equipo.')
    && sends.some(s => s.chat === '19:abc@thread.v2' && s.mentions.join() === 'Victoria Gómez') && sends.some(s => s.chat === '19:def@thread.v2' && s.mentions.length === 0), JSON.stringify(sends));
  check(`runs ${label}: each destination keeps its outcome on screen after the sends`, await page.locator('#run-send-panel .send-outcome[data-done="sent"]').count() === 2 && await page.locator('#run-send-panel .actions button:not(.secondary)').isHidden());
  // A send to the team chat whose result Teams did not confirm, as the host would have recorded it.
  await page.evaluate(() => window.__ptaMarkUncertain('3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e03', '19:abc@thread.v2'));
  await page.locator('#run-send-panel button', { hasText: 'Cerrar' }).click();
  await page.waitForFunction(() => /4 envíos/.test(document.querySelector('#run-detail-content')?.textContent || ''));
  check(`runs ${label}: the send records show on the run`, /4 envíos/.test(await detailText()));
  // A chat whose earlier send of this summary is uncertain is left out unless the person sends again.
  await page.click('#run-send');
  await page.waitForSelector('dialog.destination-picker');
  await page.locator('dialog.destination-picker .directory-picker-row', { hasText: 'Payments team' }).locator('input').check();
  await page.locator('dialog.destination-picker button', { hasText: 'Continuar' }).click();
  await page.waitForSelector('#run-send-panel [data-chat="19:abc@thread.v2"] .callout.warn');
  const before = (await calls()).filter(c => c.method === 'workflow_summary_send').length;
  await page.locator('#run-send-panel button', { hasText: 'Confirmar y enviar' }).click();
  await page.locator('#run-send-panel button', { hasText: 'Enviar ahora a 1 chat(s)' }).click();
  await page.waitForFunction(() => /Quedó fuera/.test(document.querySelector('#run-send-panel')?.textContent || ''));
  check(`runs ${label}: an uncertain earlier send keeps the chat out until the person sends again`, (await calls()).filter(c => c.method === 'workflow_summary_send').length === before);
  await page.locator('#run-send-panel button', { hasText: 'Cerrar' }).click();
  // Back to the list, then the unassigned run: choose a workflow on a second press.
  await page.click('#run-detail-back');
  await page.waitForSelector('#run-rows [data-run]');
  await page.click('#run-rows [data-run="3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e02"]');
  await page.waitForSelector('#run-choose-workflow');
  check(`runs ${label}: an unassigned run shows the director's reason`, /No encaja ninguno/.test(await detailText()));
  await page.click('#run-choose-workflow');
  await page.waitForSelector('#run-assign-workflow');
  await page.selectOption('#run-assign-workflow', 'payments-features');
  await page.locator('#run-assign button', { hasText: 'Asignar' }).click();
  check(`runs ${label}: assign waits for a second press`, !(await calls()).some(c => c.method === 'workflow_run_assign'));
  await page.locator('#run-assign button', { hasText: 'Confirmar asignar' }).click();
  await page.waitForFunction(() => window.__ptaWorkflowCalls.some(c => c.method === 'workflow_run_assign'));
  await page.waitForFunction(() => document.querySelector('#run-detail-content .run-head .badge')?.textContent === 'Planificando');
  check(`runs ${label}: the workflow is assigned and the run plans`, (await calls()).some(c => c.method === 'workflow_run_assign' && c.workflow === 'payments-features' && c.confirmed));
  // The run that needs attention: resume, then cancel, each on a second press.
  await page.click('#run-detail-back');
  await page.waitForSelector('#run-rows [data-run]');
  await page.click('#run-rows [data-run="3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e04"]');
  await page.waitForSelector('#run-attention');
  check(`runs ${label}: a run that needs attention says why`, /permission prompt/.test(await detailText()));
  const stoppedWhy = await page.locator('#run-failure').textContent().catch(() => '');
  check(`runs ${label}: the cause its machine reported is shown in the user's language`, /^Por qué se detuvo .*solicitud de permiso/.test(stoppedWhy ?? '') && /solicitud de permiso/.test(await page.locator('.run-child-failure').first().textContent()), stoppedWhy);
  await page.locator('.run-actions button', { hasText: 'Reanudar' }).click();
  await page.locator('.run-actions button', { hasText: 'Confirmar reanudar' }).click();
  await page.waitForFunction(() => window.__ptaWorkflowCalls.some(c => c.method === 'workflow_run_resume'));
  await page.waitForFunction(() => document.querySelector('#run-detail-content .run-head .badge')?.textContent === 'Programando');
  check(`runs ${label}: resume continues the run`, (await calls()).some(c => c.method === 'workflow_run_resume' && c.confirmed));
  await page.locator('.run-actions button', { hasText: 'Cancelar ejecución' }).click();
  await page.locator('.run-actions button', { hasText: 'Confirmar cancelar' }).click();
  await page.waitForFunction(() => window.__ptaWorkflowCalls.some(c => c.method === 'workflow_run_cancel'));
  await page.waitForFunction(() => document.querySelector('#run-detail-content .run-head .badge')?.textContent === 'Cancelada');
  check(`runs ${label}: cancel ends the run`, (await calls()).some(c => c.method === 'workflow_run_cancel' && c.confirmed) && await page.locator('.run-actions button', { hasText: 'Cancelar ejecución' }).count() === 0);
  // The gate from Home: the waiting run's row opens its detail, Review plan shows the children, a second press creates them.
  await page.evaluate(() => showTab('overview'));
  await page.waitForSelector('#workflow-attention [data-run]');
  await page.click('#workflow-attention [data-run="3c5f1c9a-0d2e-4b7a-8f10-5a6b7c8d9e01"] .run-open');
  await page.waitForSelector('#run-review-plan');
  check(`runs ${label}: Home's row opens the waiting run with Review plan`, await page.isVisible('#workflow-run-detail') && /Plan por revisar/.test(await detailText()));
  await page.click('#run-review-plan');
  await page.waitForSelector('#run-plan-review fieldset');
  check(`runs ${label}: Review plan shows the planning run's children`, await page.locator('#run-plan-review fieldset').count() === 2);
  await page.locator('#run-plan-review fieldset').nth(1).locator('input[type=checkbox]').uncheck();
  const gateScan = await scan(page);
  check(`runs ${label}: the plan review passes WCAG 2.2 AA`, gateScan.length === 0, JSON.stringify(gateScan));
  await page.screenshot({ path: `${out}/${label}-workflow-run-gate.png`, fullPage: true });
  await page.locator('#run-plan-review button', { hasText: 'Crear los hijos seleccionados' }).click();
  check(`runs ${label}: nothing is created before the second press`, !(await page.evaluate(() => window.__ptaLastChildCreation)));
  await page.locator('#run-plan-review button', { hasText: 'Confirmar creación de hijos' }).click();
  await page.waitForFunction(() => window.__ptaLastChildCreation);
  await page.waitForFunction(() => ['Creando los hijos', 'Programando'].includes(document.querySelector('#run-detail-content .run-head .badge')?.textContent));
  const creation = await page.evaluate(() => window.__ptaLastChildCreation);
  check(`runs ${label}: confirming creates the selected children through implementation_create and the run goes on`, creation?.confirmed && creation.id === 'a7d0c2f4-5e6b-4c8d-9f01-2a3b4c5d6e7f' && creation.children.length === 1
    && creation.item.id === 5003 && creation.item.provider === 'azure' && creation.item.task === false, JSON.stringify(creation));
  // The To-Do card of an item with an active run links to it.
  await page.evaluate(() => showTab('todo'));
  await page.waitForSelector('#todo-list .todo-item');
  await page.waitForSelector('#todo-list [data-run]');
  check(`runs ${label}: a To-Do card shows its item's active run`, await page.locator('#todo-list [data-run]').count() >= 1);
  await page.locator('#todo-list [data-run]').first().click();
  await page.waitForSelector('#run-detail-content #run-timeline');
  check(`runs ${label}: the card's link opens the run`, await page.isVisible('#workflow-run-detail'));
  check(`runs ${label}: no client errors`, errors.length === 0, errors.join('; '));
  await context.close();
}
await browser.close();
fs.writeFileSync(`${out}/flows.json`, JSON.stringify(results, null, 1));
const failed = results.filter(r => !r.ok).length;
console.log(`${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
