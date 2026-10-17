// The landing's and the README's screenshots (site/public/assets/app-*.jpg), from the preview harness:
// synthetic data only, never a real profile, with the interface in English like the landing.
// Usage: node scripts/ui-preview/server.mjs "" 4173 &   then   node scripts/ui-preview/site-shots.mjs 4173 site/public/assets
import fs from 'node:fs';
import { launch } from './deps.mjs';

const [port, out] = process.argv.slice(2);
const browser = await launch();
const answer = [
  'Two pieces of work from the last two weeks have no task yet:',
  '',
  '| Work | Day | Where it could go |',
  '| --- | --- | --- |',
  '| PR !4812 Receipt PDF renderer (portal) | Oct 13 | HU #4321 Receipt PDF for card payments |',
  '| Settlement retry runbook (Wiki) | Oct 9 | HU #4410 Timeout on settlement batch |',
  '',
  'Everything else is already registered. Reply **confirmo** in your personal chat and I will create both tasks in Done.',
].join('\n');
const chat = { answer, used_sources: ['ado-activity', 'wiki'], references: [
  { id: 'ado-activity', label: 'PR !4812 Receipt PDF renderer', url: 'https://dev.azure.com/contoso/Payments/_git/portal/pullrequest/4812' },
  { id: 'wiki', label: 'Settlement retry runbook', url: 'https://dev.azure.com/contoso/Payments/_wiki/wikis/Payments.wiki/12' }], partial: false, warnings: [] };
async function page(scheme, viewport, scale, mobile) {
  const context = await browser.newContext({ viewport, deviceScaleFactor: scale, colorScheme: scheme, locale: 'en-US', isMobile: mobile, hasTouch: mobile });
  await context.addInitScript(chat => { try { localStorage.setItem('pta-language', 'en'); } catch {} window.__ptaChatAnswer = chat; }, chat);
  const p = await context.newPage();
  await p.route('**/__fixture.js', async route => {
    const response = await route.fetch();
    route.fulfill({ response, body: (await response.text()).replace('"language": "es"', '"language": "en"').replace(/"host_version": "[^"]+"/, '"host_version": "0.7.27"') });
  });
  await p.route('**/__mock.js', async route => {
    const response = await route.fetch();
    route.fulfill({ response, body: (await response.text()).replace('chat: async () => {', 'chat: async () => { if (window.__ptaChatAnswer) { await delay(300); return window.__ptaChatAnswer; }') });
  });
  await p.goto(`http://127.0.0.1:${port}/?scenario=full`);
  await p.waitForTimeout(900);
  return { context, p };
}
const shoot = (p, name) => p.screenshot({ path: `${out}/${name}.jpg`, type: 'jpeg', quality: 84 });
for (const scheme of ['light', 'dark']) {
  // Desktop: Home, then a test chat with an activity review.
  let { context, p } = await page(scheme, { width: 1280, height: 800 }, 1, false);
  await shoot(p, `app-overview-${scheme}`);
  await p.evaluate(() => showTab('chat'));
  for (const id of ['ado-activity', 'wiki']) await p.check(`#chat-sources input[value="${id}"]`);
  await p.fill('#message', 'What work did I do in the last two weeks that no task records?');
  await p.click('#send');
  await p.waitForTimeout(900);
  await shoot(p, `app-chat-${scheme}`);
  await context.close();
  // Phone: Home and Activities.
  ({ context, p } = await page(scheme, { width: 393, height: 852 }, 2, true));
  await shoot(p, `app-phone-home-${scheme}`);
  await p.click('nav [data-tab="registration"]');
  await p.waitForTimeout(500);
  await shoot(p, `app-phone-activities-${scheme}`);
  await context.close();
}
await browser.close();
for (const f of fs.readdirSync(out)) console.log(f, fs.statSync(`${out}/${f}`).size);
