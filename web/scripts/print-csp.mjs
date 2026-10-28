import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { productionCsp } from '../csp.mjs';
const origin = new URL(process.argv[2]);
if (origin.protocol !== 'https:' || origin.pathname !== '/' || origin.search || origin.hash) throw new Error('Expected an HTTPS origin');
const shell = readFileSync(new URL('../build/200.html', import.meta.url), 'utf8');
const scriptHashes = [...shell.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)].map(m => `sha256-${createHash('sha256').update(m[1]).digest('base64')}`);
process.stdout.write(productionCsp({ websocketOrigin: `wss://${origin.host}`, scriptHashes }) + '\n');
