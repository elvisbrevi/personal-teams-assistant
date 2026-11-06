// The GUI's type check (docs/ui-design-conventions.md §11), with TypeScript installed outside the
// repository (it has no Node dependencies):
//   npm install --prefix "${UI_TYPES_DEPS:-/tmp/pta-ui-types}" typescript@6.0.3
//   node scripts/ui-types/check.mjs
// Every module is checked as `tsc -p scripts/ui-types/jsconfig.json` checks it (`--noEmit --checkJs`
// with the loose rules a view module follows); the modules in TYPED are also checked with every
// strict rule, so a parameter, binding or reply in them can never be untyped.
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

/** The modules typed in full: the transport, the control contract, the shared state and translation. */
export const TYPED = ['transport.js', 'control.js', 'state.js', 'i18n.js', 'catalog.js'];

const dir = process.env.UI_TYPES_DEPS || path.join(os.tmpdir(), 'pta-ui-types');
const ts = createRequire(path.join(dir, 'node_modules', 'noop.js'))('typescript');
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const project = path.join(here, 'jsconfig.json');
const { config } = ts.readConfigFile(project, ts.sys.readFile);
const parsed = ts.parseJsonConfigFileContent(config, ts.sys, path.dirname(project));
const host = ts.createCompilerHost(parsed.options);
const format = diagnostics => ts.formatDiagnosticsWithColorAndContext(diagnostics, { ...host, getNewLine: () => '\n' });

const loose = ts.getPreEmitDiagnostics(ts.createProgram(parsed.fileNames, parsed.options));
const strictOptions = { ...parsed.options, strict: true, noImplicitAny: true, strictNullChecks: true, noImplicitReturns: true };
const typed = new Set(TYPED.map(file => path.join(root, 'desktop/ui', file)));
const strict = ts.getPreEmitDiagnostics(ts.createProgram(parsed.fileNames, strictOptions))
  .filter(diagnostic => diagnostic.file && typed.has(path.resolve(diagnostic.file.fileName)));
// No module opts out of the check, whole or in part.
const opted = parsed.fileNames.filter(file => /@ts-(nocheck|ignore|expect-error)/.test(ts.sys.readFile(file)));
if (opted.length) console.error(`Checking disabled in: ${opted.map(file => path.relative(root, file)).join(', ')}`);
if (loose.length) console.error(`Every module (scripts/ui-types/jsconfig.json):\n${format(loose)}`);
if (strict.length) console.error(`Typed in full (${TYPED.join(', ')}), strict:\n${format(strict)}`);
if (loose.length || strict.length || opted.length) process.exit(1);
console.log(`GUI types: ${parsed.fileNames.length} files checked; ${TYPED.length} typed in full under strict rules.`);
