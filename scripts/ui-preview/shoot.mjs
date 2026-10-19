// Full-page screenshots of every section in light, dark, fresh-profile, phone and tablet layouts.
// Usage: node scripts/ui-preview/shoot.mjs <port> <out-dir> [sections...]   (server.mjs must be serving)
import fs from 'node:fs';
import { launch } from './deps.mjs';

const [port, out, ...only] = process.argv.slice(2);
fs.mkdirSync(out, { recursive: true });
const browser = await launch();
const tabs = only.length ? only : ['overview', 'todo', 'messages', 'registration', 'knowledge', 'chat', 'settings'];
const problems = [];
const runs = [
  { name: 'desktop', viewport: { width: 1440, height: 900 }, scheme: 'light', scenario: 'full' },
  { name: 'desktop-dark', viewport: { width: 1440, height: 900 }, scheme: 'dark', scenario: 'full' },
  { name: 'fresh', viewport: { width: 1440, height: 900 }, scheme: 'light', scenario: 'fresh' },
  { name: 'phone', viewport: { width: 393, height: 852 }, scheme: 'light', scenario: 'full', mobile: true },
  { name: 'tablet', viewport: { width: 820, height: 1000 }, scheme: 'light', scenario: 'full' },
];
for (const run of runs) {
  const context = await browser.newContext({ viewport: run.viewport, colorScheme: run.scheme, locale: 'es-CL', deviceScaleFactor: 1, isMobile: !!run.mobile, hasTouch: !!run.mobile });
  const page = await context.newPage();
  page.on('console', msg => { if (['error', 'warning'].includes(msg.type())) problems.push(`${run.name}: console ${msg.type()}: ${msg.text()}`); });
  page.on('pageerror', err => problems.push(`${run.name}: pageerror: ${err.message}`));
  await page.goto(`http://127.0.0.1:${port}/?scenario=${run.scenario}`);
  await page.waitForTimeout(700);
  for (const tab of tabs) {
    if (run.name !== 'desktop' && run.name !== 'phone' && !['overview', 'todo', 'settings', 'registration'].includes(tab)) continue;
    const button = page.locator(`nav [data-tab="${tab}"]`).first();
    if (await button.isVisible()) await button.click(); else await page.evaluate(t => showTab(t), tab);
    await page.waitForTimeout(450);
    await page.screenshot({ path: `${out}/${run.name}-${tab}.png`, fullPage: true });
  }
  await context.close();
}
await browser.close();
fs.writeFileSync(`${out}/problems.txt`, problems.join('\n'));
console.log(problems.length ? problems.join('\n') : 'no console problems');
