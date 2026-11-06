// Serves desktop/ui (or another copy of it) as the owner's local page over a mocked host, with
// synthetic data and the portal's CSP, so the GUI can be seen and tested without a host, a profile
// or credentials.
// The page reloads itself when a file of the GUI directory changes (Server-Sent Events, allowed by
// the CSP as a same-origin connection).
// Usage: node scripts/ui-preview/server.mjs [ui-dir] [port]   then open http://127.0.0.1:4173/?scenario=full|fresh
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const ui = path.resolve(process.argv[2] || path.join(here, '../../desktop/ui'));
const port = Number(process.argv[3] || 4173);
// The same policy the web portal sends (src/app/web.rs): a violation shows up in the console.
const csp = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";
// Open pages waiting for a change; a burst of writes (an editor's save) reloads them once.
const listeners = new Set();
let pending;
fs.watch(ui, { recursive: true }, () => {
  clearTimeout(pending);
  pending = setTimeout(() => { for (const res of listeners) res.write('data: changed\n\n'); }, 100);
});
const reload = "new EventSource('/__events').onmessage = () => window.location.reload();\n";
const types = { '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.svg': 'image/svg+xml', '.png': 'image/png', '.webmanifest': 'application/manifest+json' };

http.createServer((req, res) => {
  const { pathname } = new URL(req.url, 'http://localhost');
  let file = pathname;
  let body;
  if (pathname === '/__events') {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' });
    res.write(': waiting for changes\n\n');
    listeners.add(res);
    req.on('close', () => listeners.delete(res));
    return;
  }
  if (pathname === '/' || pathname === '/index.html') {
    file = 'index.html';
    // The mock must exist before transport.js picks its transport (classic scripts run before any
    // module); the harness's module, after the entry, exposes what the checks reach. The document
    // is marked the way the owner's local page marks it (src/app/page.rs).
    body = fs.readFileSync(path.join(ui, file), 'utf8');
    const entry = body.match(/<script type="module" src="app\.js"[^>]*><\/script>/)?.[0];
    if (!entry) { res.writeHead(500); res.end(`${file} no longer loads app.js as a module`); return; }
    body = body
      .replace(entry, `<script src="/__fixture.js"></script><script src="/__mock.js"></script><script src="/__reload.js"></script>${entry}<script type="module" src="/__expose.js"></script>`)
      .replace('<head>', '<head>\n  <meta name="pta-transport" content="local">');
  } else if (pathname === '/__reload.js') {
    body = reload;
    file = 'reload.js';
  } else if (pathname === '/__mock.js') {
    body = fs.readFileSync(path.join(here, 'mock.js'));
  } else if (pathname === '/__expose.js') {
    body = fs.readFileSync(path.join(here, 'expose.js'));
    file = 'expose.js';
  } else if (pathname === '/__fixture.js') {
    const fixture = JSON.parse(fs.readFileSync(path.join(here, 'fixture.json'), 'utf8'));
    fixture.snapshot.host_version = fs.readFileSync(path.join(here, '../../Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)[1];
    body = `window.__PTA_FIXTURE__ = ${JSON.stringify(fixture)};`;
    file = 'fixture.js';
  } else {
    const target = path.join(ui, path.normalize(pathname));
    if (!target.startsWith(ui + path.sep) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
    body = fs.readFileSync(target);
  }
  res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream', 'Content-Security-Policy': csp, 'Cache-Control': 'no-store' });
  res.end(body);
}).listen(port, '127.0.0.1', () => console.log(`GUI preview of ${ui} at http://127.0.0.1:${port}/?scenario=full`));
