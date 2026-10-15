// Playwright and axe-core stay outside the repository (it has no Node dependencies):
//   npm install --prefix "${UI_PREVIEW_DEPS:-/tmp/pta-ui-preview}" playwright-core axe-core
// PTA_UI_CHROMIUM may point at a Chromium executable; otherwise Playwright uses its own browsers.
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';

const dir = process.env.UI_PREVIEW_DEPS || path.join(os.tmpdir(), 'pta-ui-preview');
const load = createRequire(path.join(dir, 'node_modules', 'noop.js'));
export const { chromium } = load('playwright-core');
export const axePath = () => load.resolve('axe-core/axe.min.js');
export const launch = () => chromium.launch(process.env.PTA_UI_CHROMIUM ? { executablePath: process.env.PTA_UI_CHROMIUM } : {});
