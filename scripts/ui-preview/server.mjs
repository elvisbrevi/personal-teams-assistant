// Serves desktop/ui (or another copy of it) with a mocked Tauri bridge, synthetic data and the
// portal's CSP, so the GUI can be seen and tested without a host, a profile or credentials.
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
const types = { '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.svg': 'image/svg+xml', '.png': 'image/png', '.webmanifest': 'application/manifest+json' };

http.createServer((req, res) => {
  const { pathname } = new URL(req.url, 'http://localhost');
  let file = pathname;
  let body;
  if (pathname === '/' || pathname === '/index.html') {
    file = 'index.html';
    // The mock must exist before transport.js picks the Tauri bridge.
    body = fs.readFileSync(path.join(ui, file), 'utf8')
      .replace('<script src="transport.js"', '<script src="/__fixture.js"></script><script src="/__mock.js"></script><script src="transport.js"');
  } else if (pathname === '/__mock.js') {
    body = fs.readFileSync(path.join(here, 'mock.js'));
  } else if (pathname === '/__fixture.js') {
    body = `window.__PTA_FIXTURE__ = ${fs.readFileSync(path.join(here, 'fixture.json'), 'utf8')};`;
    file = 'fixture.js';
  } else {
    const target = path.join(ui, path.normalize(pathname));
    if (!target.startsWith(ui + path.sep) || !fs.existsSync(target)) { res.writeHead(404); res.end(); return; }
    body = fs.readFileSync(target);
  }
  res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream', 'Content-Security-Policy': csp, 'Cache-Control': 'no-store' });
  res.end(body);
}).listen(port, '127.0.0.1', () => console.log(`GUI preview of ${ui} at http://127.0.0.1:${port}/?scenario=full`));
