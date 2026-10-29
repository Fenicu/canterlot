import { afterEach, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb';
import { openDb, retireStorage } from '../../../src/lib/storage/idb';
import { bindOwner } from '../../../src/lib/storage/owner';

const modules = import.meta.glob('../../../src/lib/pwa/*.ts');
type PwaModules = {
  'sw-routing': typeof import('../../../src/lib/pwa/sw-routing'); update: typeof import('../../../src/lib/pwa/update');
  install: typeof import('../../../src/lib/pwa/install'); share: typeof import('../../../src/lib/pwa/share');
  signout: typeof import('../../../src/lib/pwa/signout'); csp: typeof import('../../../src/lib/pwa/csp');
};
const load = <T extends keyof PwaModules>(name: T) => modules[`../../../src/lib/pwa/${name}.ts`]!() as Promise<PwaModules[T]>;
afterEach(() => { retireStorage(); vi.unstubAllGlobals(); vi.restoreAllMocks(); localStorage.clear(); });
function storage() {
  vi.stubGlobal('indexedDB', new IDBFactory()); vi.stubGlobal('IDBKeyRange', IDBKeyRange);
}

test('manifest_is_generated_from_tokens_and_valid', async () => {
  const { manifest } = await import('../../../scripts/gen-manifest.mjs');
  const css = readFileSync('src/lib/styles/tokens.css', 'utf8');
  const m = manifest(css);
  expect(JSON.parse(readFileSync('build/manifest.webmanifest', 'utf8'))).toEqual(m);
  expect(m).toMatchObject({ id: '/', start_url: '/', scope: '/', name: 'canterlot', short_name: 'canterlot', lang: 'ru', display: 'standalone', theme_color: css.match(/--bg:\s*([^;]+)/)?.[1], background_color: css.match(/--bg:\s*([^;]+)/)?.[1], shortcuts: [{ name: 'Новая сессия', url: '/new' }] });
  expect(m.share_target).toEqual({ action: '/_share', method: 'POST', enctype: 'multipart/form-data', params: { title: 'title', text: 'text', url: 'url', files: [{ name: 'files', accept: ['*/*'] }] } });
  expect(m.share_target.action.startsWith(m.scope)).toBe(true);
  expect(m.icons).toHaveLength(3);
  for (const icon of m.icons) {
    const png = readFileSync(`static${icon.src}`);
    expect(`${png.readUInt32BE(16)}x${png.readUInt32BE(20)}`).toBe(icon.sizes);
  }
});

test('icons_are_rasterised_from_the_svg_with_token_colours', async () => {
  const { colouredSvg } = await import('../../../scripts/gen-icons.mjs');
  const svg = colouredSvg(readFileSync('static/icons/source.svg', 'utf8'), readFileSync('src/lib/styles/tokens.css', 'utf8'));
  expect(svg).not.toContain('{{');
  expect(svg).toContain('#183135'); expect(svg).toContain('#00f4ee');
  const script = readFileSync('scripts/gen-icons.mjs', 'utf8');
  expect(script).toContain('chromium'); expect(script).toContain('screenshot');
  // @ts-expect-error pngjs используется только для проверки сгенерированных файлов.
  const { PNG } = await import('pngjs');
  const png = PNG.sync.read(readFileSync('static/icons/icon-maskable-512.png'));
  expect([...png.data.subarray(0, 4)]).toEqual([24, 49, 53, 255]);
  expect(new Set(Array.from({ length: png.width * png.height }, (_, i) => png.data.subarray(i * 4, i * 4 + 4).toString('hex'))).has('00f4eeff')).toBe(true);
  for (const size of [192, 512]) {
    const icon = PNG.sync.read(readFileSync(`static/icons/icon-${size}.png`));
    const dot = (Math.floor(size / 2) * size + Math.floor(size * 336 / 512)) * 4;
    expect([...icon.data.subarray(dot, dot + 4)]).toEqual([0, 244, 238, 255]);
  }
});

test('sw_classify_never_handles_api_or_mutations', async () => {
  const { classify } = await load('sw-routing');
  for (const [url, method, mode, destination, expected] of [
    ['/api/v1/me', 'GET', 'navigate', '', 'api'], ['/api/v1/attachments/a/content', 'PATCH', 'cors', '', 'api'],
    ['/healthz', 'GET', 'navigate', '', 'api'], ['/_share', 'POST', 'cors', '', 'share_target'],
    ['/new', 'POST', 'navigate', '', 'passthrough'], ['/x.js', 'PUT', 'cors', 'script', 'passthrough'],
    ['/c/123', 'GET', 'navigate', 'document', 'navigation'], ['/_app/immutable/a.js', 'GET', 'cors', 'script', 'asset'],
    ['/private.png', 'GET', 'cors', 'image', 'passthrough'], ['/config', 'GET', 'cors', '', 'api']
  ] as const) expect(classify({ url: `https://app.test${url}`, method, mode, destination })).toBe(expected);
});

test('navigation_fallback_serves_the_shell_only_for_get_navigations_when_the_network_fails', async () => {
  const { navigationResponse, cacheable } = await load('sw-routing');
  const shell = new Response('shell'); const cached = vi.fn(async () => shell);
  const down = vi.fn(async () => { throw new TypeError('offline'); });
  const request = { url: 'https://app.test/c/123', method: 'GET', mode: 'navigate', destination: 'document' };
  expect(await navigationResponse(request, down, cached)).toBe(shell);
  const denied = new Response('login', { status: 401 });
  expect(await navigationResponse(request, async () => denied, cached)).toBe(denied);
  for (const patch of [{ method: 'POST' }, { mode: 'cors' }, { url: 'https://app.test/api/v1/me' }]) {
    await expect(navigationResponse({ ...request, ...patch }, down, cached)).rejects.toThrow();
  }
  for (const response of [denied, { ok: true, type: 'opaqueredirect' }, { ok: true, type: 'basic', redirected: true }, new Response('private', { headers: { 'Cache-Control': 'private' } })]) expect(cacheable(response as Response)).toBe(false);
  expect(cacheable(new Response('asset'))).toBe(true);
});

function registration() {
  const reg = Object.assign(new EventTarget(), { waiting: { postMessage: vi.fn() }, installing: null, update: vi.fn(async () => {}) });
  const worker = new EventTarget(); const reload = vi.fn();
  return { reg, worker, reload };
}
test('update_controller_waits_for_a_safe_moment_and_never_reloads_during_typing', async () => {
  const { UpdateController } = await load('update');
  const { reg, worker, reload } = registration(); let safe = false;
  const flushDrafts = vi.fn(async () => {});
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration, { isSafe: () => safe, hidden: () => false, flushDrafts, serviceWorker: worker, reload });
  await ctl.check(); expect(ctl.state).toBe('ready');
  await ctl.apply(); expect(reg.waiting.postMessage).not.toHaveBeenCalled(); expect(reload).not.toHaveBeenCalled();
  safe = true; await ctl.apply(); expect(flushDrafts).toHaveBeenCalled(); expect(reg.waiting.postMessage).toHaveBeenCalledWith({ type: 'SKIP_WAITING' });
  safe = false; worker.dispatchEvent(new Event('controllerchange')); expect(reload).not.toHaveBeenCalled(); expect(ctl.state).toBe('ready');
  safe = true; await ctl.apply(); expect(reload).toHaveBeenCalledOnce(); ctl.destroy();
});
test('update_applies_after_drafts_are_flushed_when_hidden', async () => {
  const { UpdateController } = await load('update'); const { reg, worker, reload } = registration();
  let finish!: () => void;
  const flush = vi.fn(async () => {}).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration, { isSafe: () => false, hidden: () => true, flushDrafts: flush, serviceWorker: worker, reload });
  const applying = ctl.apply(); expect(reg.waiting.postMessage).not.toHaveBeenCalled(); await vi.waitFor(() => expect(finish).toBeTypeOf('function')); finish(); await applying;
  worker.dispatchEvent(new Event('controllerchange')); await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
  expect(reg.waiting.postMessage).toHaveBeenCalledOnce(); expect(reload).toHaveBeenCalledOnce(); ctl.destroy();
});
test('update_flushes_new_text_again_before_a_hidden_controllerchange_reload', async () => {
  const { UpdateController } = await load('update'); const { reg, worker, reload } = registration();
  let hidden = false; let finish!: () => void;
  const flush = vi.fn(async () => {}).mockImplementationOnce(async () => {}).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve; }));
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration, { isSafe: () => !hidden, hidden: () => hidden, flushDrafts: flush, serviceWorker: worker, reload });
  await ctl.apply(); hidden = true; worker.dispatchEvent(new Event('controllerchange'));
  expect(reload).not.toHaveBeenCalled(); await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
  finish(); await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce()); ctl.destroy();
});
test('update_request_survives_installing_and_rechecks_safety_before_activation', async () => {
  const { UpdateController } = await load('update');
  const candidate = Object.assign(new EventTarget(), { state: 'installing', postMessage: vi.fn() });
  const reg = Object.assign(new EventTarget(), { waiting: null, installing: candidate, update: vi.fn(async () => {}) });
  let safe = true;
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration, { isSafe: () => safe, hidden: () => false, flushDrafts: async () => {} });
  expect(ctl.state).toBe('available'); await ctl.apply();
  safe = false; candidate.state = 'installed'; candidate.dispatchEvent(new Event('statechange'));
  await Promise.resolve(); expect(ctl.state).toBe('ready'); expect(candidate.postMessage).not.toHaveBeenCalled();
  safe = true; await ctl.apply(); expect(candidate.postMessage).toHaveBeenCalledWith({ type: 'SKIP_WAITING' }); ctl.destroy();
});
test('incompatible_schema_version_prompts_an_update', async () => {
  const { UpdateController } = await load('update'); const { reg } = registration();
  reg.waiting = null as unknown as typeof reg.waiting;
  const ctl = new UpdateController(reg as unknown as ServiceWorkerRegistration, { isSafe: () => false, hidden: () => false, flushDrafts: async () => {} });
  ctl.schemaVersion(2); expect(ctl.state).toBe('available'); expect(ctl.message).toBe('Доступна новая версия'); ctl.destroy();
  const signal = vi.fn(); window.addEventListener('canterlot:incompatible', signal);
  try {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ schema_version: 2, limits: {} }), { headers: { 'Content-Type': 'application/json' } })));
    const { loadConfig } = await import('../../../src/lib/api/config');
    await loadConfig(); expect(signal).toHaveBeenCalledOnce();
    expect((await load('update')).needsUpdate).toBe(true);
  } finally { window.removeEventListener('canterlot:incompatible', signal); }
});
test('install_controller_exposes_the_prompt_once_and_hides_after_install', async () => {
  const { InstallController } = await load('install'); const target = new EventTarget(); const ctl = new InstallController(target);
  const prompt = vi.fn(async () => {}); const event = Object.assign(new Event('beforeinstallprompt', { cancelable: true }), { prompt, userChoice: Promise.resolve({ outcome: 'accepted' }) });
  target.dispatchEvent(event); expect(event.defaultPrevented).toBe(true); expect(ctl.canInstall).toBe(true);
  expect(await ctl.prompt()).toBe('accepted'); expect(await ctl.prompt()).toBe('dismissed'); expect(prompt).toHaveBeenCalledOnce();
  target.dispatchEvent(event); target.dispatchEvent(new Event('appinstalled')); expect(ctl.canInstall).toBe(false); ctl.destroy();
});
test('share_inbox_stores_files_and_expires_after_24h', async () => {
  storage(); const { stashShare, takeShare, sweepShares } = await load('share'); const db = await openDb('alice');
  // Node structuredClone не сохраняет File из jsdom; браузерный IDB проверяет e2e.
  const inbox = new Map<string, unknown>();
  vi.spyOn(db, 'put').mockImplementation(async (_store, key, value) => { inbox.set(key, value); });
  vi.spyOn(db, 'get').mockImplementation(async (_store, key) => inbox.get(key) as never);
  vi.spyOn(db, 'list').mockImplementation(async () => [...inbox.values()] as never);
  vi.spyOn(db, 'delete').mockImplementation(async (_store, key) => { inbox.delete(key); });
  const form = new FormData(); form.append('files', new File(['hello'], 'sample.txt', { type: 'text/plain' }));
  const id = await stashShare(db, form); const item = await takeShare(db, id);
  expect(item?.files[0]?.name).toBe('sample.txt'); expect(item?.files[0]?.size).toBe(5); expect(await takeShare(db, id)).toBeNull();
  const expired = await stashShare(db, form); expect(await sweepShares(db, Date.now() + 86400001)).toBe(1); expect(await takeShare(db, expired)).toBeNull();
});
test('share_text_and_url_go_to_the_draft_not_to_attachments', async () => {
  storage(); const { stashShare, takeShare } = await load('share'); const db = await openDb('alice');
  const form = new FormData(); form.set('title', 'Заголовок'); form.set('text', 'Текст'); form.set('url', 'https://example.org/');
  const item = await takeShare(db, await stashShare(db, form));
  expect(item).toMatchObject({ title: 'Заголовок', text: 'Текст', url: 'https://example.org/', files: [] });
});
test('signout_purges_idb_caches_local_storage_and_subscriptions', async () => {
  storage(); const { purgeLocal } = await load('signout'); const db = await openDb('alice');
  for (const store of ['drafts', 'ops', 'uploads', 'share_inbox'] as const) await db.put(store, 'x', { text: 'private' });
  localStorage.setItem('canterlot.theme', 'light'); localStorage.setItem('canterlot.draft-tail.x', 'private'); localStorage.setItem('unrelated', 'keep');
  const remove = vi.fn(async () => true); const dispose = vi.fn(); const reset = vi.fn();
  const { bindComposerRestore } = await import('../../../src/lib/composer/restore');
  let visibleText = 'Личный текст';
  const stop = bindComposerRestore(text => { visibleText = text; });
  await purgeLocal({ idb: db, caches: { keys: async () => ['canterlot-shell-v1', 'private-v0'], delete: remove }, ls: localStorage, connection: { dispose }, runtime: { dispose: reset } });
  stop(); expect(visibleText).toBe('');
  expect(dispose).toHaveBeenCalledOnce(); expect(reset).toHaveBeenCalledOnce(); expect(remove).toHaveBeenCalledWith('private-v0');
  expect(remove).not.toHaveBeenCalledWith('canterlot-shell-v1');
  expect(localStorage.getItem('canterlot.theme')).toBeNull(); expect(localStorage.getItem('canterlot.draft-tail.x')).toBeNull(); expect(localStorage.getItem('unrelated')).toBe('keep');
  const fresh = await openDb('alice'); for (const store of ['drafts', 'ops', 'uploads', 'share_inbox'] as const) expect(await fresh.list(store)).toEqual([]);
});
test('owner_change_starts_clean', async () => {
  storage(); await bindOwner('alice'); const db = await openDb('alice'); await db.put('share_inbox', 'x', { text: 'private' }); localStorage.setItem('canterlot.theme', 'light');
  await bindOwner('bob'); expect(await (await openDb('alice')).list('share_inbox')).toEqual([]); expect(localStorage.getItem('canterlot.theme')).toBeNull();
});
test('csp_directives_are_complete_and_the_built_shell_has_only_hashed_or_external_scripts', async () => {
  const { scriptHashes, scriptsAllowed } = await load('csp');
  const path = '../../../csp.mjs'; const { directives } = await import(path);
  expect(Object.keys(directives)).toEqual(['default-src', 'script-src', 'style-src', 'img-src', 'connect-src', 'worker-src', 'manifest-src', 'object-src', 'base-uri', 'form-action', 'frame-ancestors']);
  const shell = '<script src="/theme-init.js"></script><script>start()</script>';
  const hash = `sha256-${createHash('sha256').update('start()').digest('base64')}`;
  expect(await scriptHashes(shell)).toEqual([hash]); expect(await scriptsAllowed(shell, [hash])).toBe(true); expect(await scriptsAllowed(shell, [])).toBe(false); expect(await scriptsAllowed('<button onclick="x()">x</button>', [])).toBe(false);
  const built = readFileSync('build/200.html', 'utf8');
  const hashes = await scriptHashes(built);
  expect(hashes.length).toBeGreaterThan(0);
  expect(await scriptsAllowed(built, hashes)).toBe(true);
  for (const hash of hashes) expect(built).toContain(`'${hash}'`);
  expect(built).toContain('<script src="/theme-init.js"></script>');
  const bootstrapId = built.match(/(__sveltekit_\w+) =/)?.[1];
  const start = built.match(/import\("([^\"]*entry\/start[^\"]+)"\)/)?.[1];
  expect(bootstrapId).toBeTruthy(); expect(start).toBeTruthy();
  expect(built).toContain(`kit.init(${bootstrapId})`);
});
test('kit_csp_connect_src_is_self_only_and_the_header_adds_the_wss_origin', async () => {
  const path = '../../../csp.mjs'; const { directives, productionCsp } = await import(path);
  expect(directives['connect-src']).toEqual(['self']); expect(productionCsp({ websocketOrigin: 'wss://app.test' })).toContain("connect-src 'self' wss://app.test");
  expect(readFileSync('../README.md', 'utf8')).toContain('wss://<origin-host>');
});
