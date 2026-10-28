import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { resolve, extname, sep } from 'node:path';
import { createHash } from 'node:crypto';
import { productionCsp } from '../../csp.mjs';
const root = resolve(process.env.WEB_BUILD ?? 'build');
const port = Number(process.env.PORT ?? 4173);
const shell = await readFile(resolve(root, '200.html'), 'utf8');
const fixturePath = resolve('e2e/edge/fixtures/ws-smoke.html');
const fixture = await readFile(fixturePath, 'utf8');
const hashes = html => [...html.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)].map(m => `sha256-${createHash('sha256').update(m[1]).digest('base64')}`);
const scriptHashes = hashes(shell);
const csp = productionCsp({ websocketOrigin: `ws://127.0.0.1:${port}`, scriptHashes });
const fixtureCsp = productionCsp({ websocketOrigin: `ws://127.0.0.1:${port}`, scriptHashes: hashes(fixture) });
const mime = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.webmanifest': 'application/manifest+json', '.png': 'image/png', '.svg': 'image/svg+xml' };
const server = createServer(async (request, response) => {
  response.setHeader('Content-Security-Policy', csp);
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Cache-Control', 'no-cache');
  try {
    const url = new URL(request.url, `http://127.0.0.1:${port}`);
    if (request.method === 'POST' && url.pathname === '/_share') { response.writeHead(303, { Location: '/share?error=sw_inactive' }); response.end(); return; }
    if (!['GET', 'HEAD'].includes(request.method)) { response.writeHead(405); response.end(); return; }
    if (url.pathname === '/__fixtures__/ws-smoke.html') {
      response.setHeader('Content-Security-Policy', fixtureCsp);
      response.setHeader('Content-Type', mime['.html']); response.writeHead(200);
      response.end(request.method === 'HEAD' ? undefined : fixture); return;
    }
    let file = resolve(root, `.${decodeURIComponent(url.pathname)}`);
    if (file !== root && !file.startsWith(root + sep)) { response.writeHead(403); response.end(); return; }
    try { if (!(await stat(file)).isFile()) file = resolve(root, '200.html'); }
    catch { if (extname(url.pathname)) { response.writeHead(404); response.end(); return; } file = resolve(root, '200.html'); }
    const body = await readFile(file);
    response.setHeader('Content-Type', mime[extname(file)] ?? 'application/octet-stream');
    response.writeHead(200);
    response.end(request.method === 'HEAD' ? undefined : body);
  } catch { response.writeHead(400); response.end(); }
});
server.listen(port, '127.0.0.1');
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.close());
