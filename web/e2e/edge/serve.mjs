import { createServer, request as httpRequest } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { readFileSync, existsSync } from 'node:fs';
import { resolve, extname, sep } from 'node:path';
import { createHash } from 'node:crypto';
import { productionCsp } from '../../csp.mjs';
import { ChaosController, decodeWsFrame, encodeWsFrame } from './chaos.mjs';

const root = resolve(process.env.WEB_BUILD ?? 'build');
const port = Number(process.env.PORT ?? 4173);
const hubUrlStr = process.env.HUB_URL ?? 'http://127.0.0.1:8080';
const hubUrl = new URL(hubUrlStr);
const proxyOwner = process.env.PROXY_OWNER_USER ?? 'owner';
let proxySecret = process.env.PROXY_SECRET ?? '';
if (!proxySecret && process.env.PROXY_SECRET_FILE && existsSync(process.env.PROXY_SECRET_FILE)) {
  proxySecret = readFileSync(process.env.PROXY_SECRET_FILE, 'utf8').trim();
}

const chaos = new ChaosController();
const nextRoot = resolve('build-update');
const shell = await readFile(resolve(root, '200.html'), 'utf8');
const fixturePath = resolve('e2e/edge/fixtures/ws-smoke.html');
const fixture = await readFile(fixturePath, 'utf8');
const hashes = html => [...html.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)].map(m => `sha256-${createHash('sha256').update(m[1]).digest('base64')}`);
const scriptHashes = hashes(shell);
const csp = productionCsp({ websocketOrigin: `ws://127.0.0.1:${port}`, scriptHashes });
const fixtureCsp = productionCsp({ websocketOrigin: `ws://127.0.0.1:${port}`, scriptHashes: hashes(fixture) });
const mime = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.webmanifest': 'application/manifest+json',
  '.png': 'image/png',
  '.svg': 'image/svg+xml'
};

function filterClientHeaders(incoming) {
  const clean = {};
  for (const [key, val] of Object.entries(incoming)) {
    const lower = key.toLowerCase();
    if (lower.startsWith('remote-') || lower.startsWith('x-forwarded-') || lower === 'x-canterlot-proxy') {
      continue;
    }
    clean[key] = val;
  }
  clean['remote-user'] = proxyOwner;
  if (proxySecret) clean['x-canterlot-proxy'] = proxySecret;
  clean['host'] = hubUrl.host;
  return clean;
}

const server = createServer(async (request, response) => {
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Cache-Control', 'no-cache');

  try {
    const url = new URL(request.url, `http://127.0.0.1:${port}`);

    // Chaos blackhole on HTTP
    if (chaos.blackholeActive) {
      chaos.trackHttp({ response, proxyReq: null });
      return;
    }

    // Reverse proxy for /api/* and /healthz /readyz
    if (url.pathname.startsWith('/api/') || url.pathname === '/healthz' || url.pathname === '/readyz') {
      const isDropMatch = chaos.dropResponsePattern && request.method === chaos.dropResponseMethod && chaos.dropResponsePattern.test(url.pathname);
      if (isDropMatch) {
        chaos.dropResponsePattern = null;
      }

      const headers = filterClientHeaders(request.headers);
      const options = {
        hostname: hubUrl.hostname,
        port: hubUrl.port,
        path: request.url,
        method: request.method,
        headers
      };

      const entry = { response, proxyReq: null };
      chaos.trackHttp(entry);
      const sendProxyRequest = () => {
        if (response.destroyed) return;
        const proxyReq = httpRequest(options, hubRes => {
          if (isDropMatch) {
            // Запрос зафиксирован хабом, а ответ до браузера не доходит.
            hubRes.resume();
            response.destroy();
            return;
          }

          if (chaos.latencyMs > 0) {
            setTimeout(() => {
              if (response.destroyed) return;
              response.writeHead(hubRes.statusCode ?? 200, hubRes.headers);
              hubRes.pipe(response);
            }, chaos.latencyMs);
          } else {
            response.writeHead(hubRes.statusCode ?? 200, hubRes.headers);
            hubRes.pipe(response);
          }
        });

        entry.proxyReq = proxyReq;
        const cutAfter = chaos.takeCutDuring(request.method, url.pathname);
        if (cutAfter !== null) {
          let seen = 0;
          request.on('data', (chunk) => {
            seen += chunk.length;
            if (seen >= cutAfter && !response.destroyed) {
              proxyReq.destroy();
              response.destroy();
            }
          });
        }
        proxyReq.on('error', () => {
          if (!response.headersSent && !response.destroyed) {
            response.writeHead(502);
            response.end('Bad Gateway');
          }
        });

        request.pipe(proxyReq);
      };

      if (chaos.latencyMs > 0) {
        setTimeout(sendProxyRequest, chaos.latencyMs);
      } else {
        sendProxyRequest();
      }
      return;
    }

    // Static files
    response.setHeader('Content-Security-Policy', csp);
    const selectedRoot = /(?:^|;\s*)pwa-build=next(?:;|$)/.test(request.headers.cookie ?? '') ? nextRoot : root;
    if (selectedRoot === nextRoot) {
      const nextShell = await readFile(resolve(nextRoot, '200.html'), 'utf8');
      response.setHeader('Content-Security-Policy', productionCsp({ websocketOrigin: `ws://127.0.0.1:${port}`, scriptHashes: hashes(nextShell) }));
    }
    if (url.pathname === '/service-worker.js') response.setHeader('Service-Worker-Allowed', '/');
    if (request.method === 'POST' && url.pathname === '/_share') {
      response.writeHead(303, { Location: '/share?error=sw_inactive' });
      response.end();
      return;
    }
    if (!['GET', 'HEAD'].includes(request.method)) {
      response.writeHead(405);
      response.end();
      return;
    }
    if (url.pathname === '/__fixtures__/ws-smoke.html') {
      response.setHeader('Content-Security-Policy', fixtureCsp);
      response.setHeader('Content-Type', mime['.html']);
      response.writeHead(200);
      response.end(request.method === 'HEAD' ? undefined : fixture);
      return;
    }

    let file = resolve(selectedRoot, `.${decodeURIComponent(url.pathname)}`);
    if (file !== selectedRoot && !file.startsWith(selectedRoot + sep)) {
      response.writeHead(403);
      response.end();
      return;
    }
    try {
      if (!(await stat(file)).isFile()) file = resolve(selectedRoot, '200.html');
    } catch {
      if (extname(url.pathname)) {
        response.writeHead(404);
        response.end();
        return;
      }
      file = resolve(selectedRoot, '200.html');
    }
    const body = await readFile(file);
    response.setHeader('Content-Type', mime[extname(file)] ?? 'application/octet-stream');
    response.writeHead(200);
    response.end(request.method === 'HEAD' ? undefined : body);
  } catch {
    if (!response.headersSent && !response.destroyed) {
      response.writeHead(400);
      response.end();
    }
  }
});

// WebSocket Upgrade reverse proxy with Chaos pipe
server.on('connection', (socket) => {
  chaos.httpSockets.add(socket);
  socket.on('close', () => chaos.httpSockets.delete(socket));
});

server.on('upgrade', (req, clientSocket, head) => {
  const url = new URL(req.url, `http://127.0.0.1:${port}`);
  if (!url.pathname.startsWith('/api/')) {
    clientSocket.destroy();
    return;
  }

  const cleanHeaders = filterClientHeaders(req.headers);
  delete cleanHeaders['sec-websocket-extensions'];
  const options = {
    hostname: hubUrl.hostname,
    port: hubUrl.port,
    path: req.url,
    method: 'GET',
    headers: cleanHeaders
  };

  const hubReq = httpRequest(options);
  hubReq.on('upgrade', (hubRes, hubSocket, hubHead) => {
    let rawHead = `HTTP/1.1 101 Switching Protocols\r\n`;
    for (const [k, v] of Object.entries(hubRes.headers)) {
      rawHead += `${k}: ${v}\r\n`;
    }
    rawHead += `\r\n`;
    clientSocket.write(rawHead);

    if (head && head.length > 0) clientSocket.unshift(head);
    if (hubHead && hubHead.length > 0) hubSocket.unshift(hubHead);

    chaos.registerWsSocket(clientSocket, hubSocket);

    // Client -> Hub frames
    let clientBuf = Buffer.alloc(0);
    clientSocket.on('data', chunk => {
      clientBuf = Buffer.concat([clientBuf, chunk]);
      while (true) {
        const frame = decodeWsFrame(clientBuf);
        if (!frame) break;
        clientBuf = clientBuf.subarray(frame.totalLength);

        if (frame.opcode === 1 && frame.fin) {
          try {
            const op = JSON.parse(frame.payload.toString('utf8'));
            if (chaos.rewriteSubscribeDelta !== null && !chaos.wsDropped && op.op === 'subscribe' && String(op.scope ?? '').startsWith(chaos.rewriteScopePrefix)) {
              const delta = BigInt(chaos.rewriteSubscribeDelta);
              op.after = (BigInt(op.after ?? '0') + delta).toString();
              frame.payload = Buffer.from(JSON.stringify(op), 'utf8');
              chaos.rewriteSubscribeDelta = null;
            }
            chaos.logFrame('client_to_hub', op, !chaos.wsDropped);
          } catch {}
        }

        if (chaos.wsDropped) continue;
        const encoded = encodeWsFrame(frame.payload, frame.opcode, true, frame.fin);
        if (chaos.latencyMs > 0) {
          setTimeout(() => {
            if (!hubSocket.destroyed) hubSocket.write(encoded);
          }, chaos.latencyMs);
        } else {
          hubSocket.write(encoded);
        }
      }
    });

    // Hub -> Client frames
    let hubBuf = Buffer.alloc(0);
    hubSocket.on('data', chunk => {
      hubBuf = Buffer.concat([hubBuf, chunk]);
      while (true) {
        const frame = decodeWsFrame(hubBuf);
        if (!frame) break;
        hubBuf = hubBuf.subarray(frame.totalLength);

        if (frame.opcode === 1 && frame.fin) {
          try {
            const op = JSON.parse(frame.payload.toString('utf8'));
            chaos.logFrame('hub_to_client', op, !chaos.wsDropped);
          } catch {}
        }

        if (chaos.wsDropped) continue;
        const encoded = encodeWsFrame(frame.payload, frame.opcode, false, frame.fin);
        if (chaos.latencyMs > 0) {
          setTimeout(() => {
            if (!clientSocket.destroyed) clientSocket.write(encoded);
          }, chaos.latencyMs);
        } else {
          clientSocket.write(encoded);
        }
      }
    });

    const cleanup = () => {
      try { clientSocket.destroy(); } catch {}
      try { hubSocket.destroy(); } catch {}
    };
    clientSocket.on('error', cleanup);
    hubSocket.on('error', cleanup);
    // Закрытие хаба доходит до браузера, если только канал не в чёрной дыре: тогда сокет остаётся полуоткрытым, как после сна телефона.
    clientSocket.on('close', () => { try { hubSocket.destroy(); } catch {} });
    hubSocket.on('close', () => { if (!chaos.wsDropped) { try { clientSocket.end(); } catch {} } });
  });

  hubReq.on('error', () => clientSocket.destroy());
  hubReq.end();
});

server.listen(port, '127.0.0.1');

// Chaos Control server
const controlPort = Number(process.env.CONTROL_PORT || 0);
let controlServer = null;
if (controlPort > 0) {
  controlServer = createServer(async (req, res) => {
    res.setHeader('Content-Type', 'application/json');
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', '*');
    if (req.method === 'OPTIONS') {
      res.writeHead(204);
      res.end();
      return;
    }

    const u = new URL(req.url, `http://127.0.0.1:${controlPort}`);
    if (req.method === 'POST' && u.pathname === '/chaos') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const cmd = JSON.parse(body || '{}');
      const result = chaos.apply(cmd);
      res.writeHead(200);
      res.end(JSON.stringify(result));
      return;
    }
    if (req.method === 'GET' && u.pathname === '/frames') {
      res.writeHead(200);
      res.end(JSON.stringify(chaos.getFrames()));
      return;
    }
    res.writeHead(404);
    res.end('{}');
  });
  controlServer.listen(controlPort, '127.0.0.1');
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    try { server.close(); } catch {}
    if (controlServer) {
      try { controlServer.close(); } catch {}
    }
  });
}
