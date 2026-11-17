import { afterEach, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { IDBFactory, IDBKeyRange } from 'fake-indexeddb';
import { openDb, retireStorage } from '../../../src/lib/storage/idb';
import { bindOwner } from '../../../src/lib/storage/owner';

const modules = import.meta.glob('../../../src/lib/pwa/*.ts');
type PwaModules = {
  update: typeof import('../../../src/lib/pwa/update');
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
  // Заставка совпадает с фоном палитры по умолчанию — тёмной Стандартной.
  expect(m).toMatchObject({ id: '/', start_url: '/', scope: '/', name: 'Canterlot', short_name: 'Canterlot', lang: 'ru', display: 'standalone', theme_color: '#1c1c1e', background_color: '#1c1c1e', shortcuts: [{ name: 'Новая сессия', url: '/new' }] });
  expect(m).not.toHaveProperty('share_target');
  expect(m.icons).toHaveLength(3);
  for (const icon of m.icons) {
    const png = readFileSync(`static${icon.src}`);
    expect(`${png.readUInt32BE(16)}x${png.readUInt32BE(20)}`).toBe(icon.sizes);
  }
});

test('app_html_links_generated_icons_and_never_the_template', async () => {
  const html = readFileSync('src/app.html', 'utf8');
  const hrefs = [...html.matchAll(/<link\b[^>]*rel="(?:icon|apple-touch-icon)"[^>]*href="([^"]+)"/g)].map((match) => match[1]!);
  expect(hrefs).toEqual(['/icons/favicon.svg', '/icons/favicon-32.png', '/icons/apple-touch-icon.png']);
  for (const href of hrefs) {
    const body = readFileSync(`static${href}`);
    expect(body.length, href).toBeGreaterThan(0);
    expect(body.includes('{{'), `${href} остался шаблоном`).toBe(false);
  }
  // SVG-фавикон — тот же замок, что источник; PNG растеризуются из него.
  expect(readFileSync('static/icons/favicon.svg', 'utf8')).toBe(readFileSync('static/icons/source.svg', 'utf8'));
  expect(html).not.toContain('/icons/source.svg');
});

test('generated_icons_have_the_expected_sizes_and_backgrounds', async () => {
  // @ts-expect-error pngjs используется только для проверки сгенерированных файлов.
  const { PNG } = await import('pngjs');
  const png = (name: string) => PNG.sync.read(readFileSync(`static/icons/${name}`));
  const sizes = { 'favicon-32.png': 32, 'apple-touch-icon.png': 180, 'icon-192.png': 192, 'icon-512.png': 512, 'icon-maskable-512.png': 512 };
  for (const [name, size] of Object.entries(sizes)) {
    const icon = png(name);
    expect([name, icon.width, icon.height]).toEqual([name, size, size]);
  }
  expect(readFileSync('static/icons/favicon.svg', 'utf8')).toContain('#141a2e');
  expect(readFileSync('static/icons/favicon.svg', 'utf8')).toContain('#f2c14e');
  // iOS скругляет сам: фон apple-touch-icon квадратный, непрозрачный и во весь значок.
  const touch = png('apple-touch-icon.png');
  expect([...touch.data.subarray(0, 4)]).toEqual([20, 26, 46, 255]);
  expect([...touch.data.subarray((180 * 179 + 179) * 4)]).toEqual([20, 26, 46, 255]);
  // Обычные значки стоят на прозрачных углах скруглённого фона.
  const rounded = png('icon-192.png');
  expect(rounded.data[3]).toBe(0);
  expect([...rounded.data.subarray((10 * 192 + 96) * 4, (10 * 192 + 96) * 4 + 4)]).toEqual([20, 26, 46, 255]);
});

test('maskable_icon_keeps_the_castle_inside_the_safe_circle', async () => {
  // @ts-expect-error pngjs используется только для проверки сгенерированных файлов.
  const { PNG } = await import('pngjs');
  const png = PNG.sync.read(readFileSync('static/icons/icon-maskable-512.png'));
  const background = [20, 26, 46, 255];
  const center = png.width / 2;
  let minX = Infinity; let minY = Infinity; let maxX = -1; let maxY = -1; let maxRadius = 0;
  for (let y = 0; y < png.height; y += 1) {
    for (let x = 0; x < png.width; x += 1) {
      const i = (y * png.width + x) * 4;
      if (background.every((value, channel) => png.data[i + channel] === value)) continue;
      minX = Math.min(minX, x); minY = Math.min(minY, y);
      maxX = Math.max(maxX, x); maxY = Math.max(maxY, y);
      maxRadius = Math.max(maxRadius, Math.hypot(x + 0.5 - center, y + 0.5 - center));
    }
  }
  const safe = png.width * 0.4;
  expect(maxX).toBeGreaterThan(minX);
  expect(maxRadius, `замок ${minX},${minY}..${maxX},${maxY} выходит из круга ${safe}`).toBeLessThanOrEqual(safe);
  expect(Math.min(minX, minY)).toBeGreaterThan(0);
  expect(Math.max(maxX, maxY)).toBeLessThan(png.width - 1);
});

test('incompatible_schema_version_prompts_an_update', async () => {
  const { UpdateController } = await load('update');
  const ctl = new UpdateController({ isSafe: () => false, hidden: () => false, flushDrafts: async () => {} });
  ctl.incompatible(); expect(ctl.state).toBe('available'); expect(ctl.message).toBe('Доступна новая версия'); ctl.destroy();
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
  expect(remove).toHaveBeenCalledWith('canterlot-shell-v1');
  expect(localStorage.getItem('canterlot.theme')).toBeNull(); expect(localStorage.getItem('canterlot.draft-tail.x')).toBeNull(); expect(localStorage.getItem('unrelated')).toBe('keep');
  const fresh = await openDb('alice'); for (const store of ['drafts', 'ops', 'uploads', 'share_inbox'] as const) expect(await fresh.list(store)).toEqual([]);
});
test('signout_in_another_window_stops_writers_purges_and_reloads_once', async () => {
  storage(); const { announceSignout, watchSignout, followSignout } = await load('signout');
  const { DraftStore } = await import('../../../src/lib/drafts/store');
  const db = await openDb('alice'); const drafts = new DraftStore('alice', db, { debounceMs: 0 });
  drafts.set('c', { text: 'Личный черновик' }); await drafts.flush();
  localStorage.setItem('canterlot:current_owner', 'alice'); localStorage.setItem('canterlot.theme', 'light');
  const reload = vi.fn();
  const stop = watchSignout(() => { void followSignout({ ls: localStorage, reload }); });
  try {
    announceSignout();
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(reload).not.toHaveBeenCalled();
    const foreign = new BroadcastChannel('canterlot'); foreign.postMessage({ type: 'signout', from: 'other-window' }); foreign.close();
    await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
    window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot:current_owner', oldValue: 'alice', newValue: null }));
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(reload).toHaveBeenCalledOnce();
    drafts.set('c', { text: 'Записано после выхода' }); drafts.flushSync();
    expect(Object.keys(localStorage).filter(key => key.startsWith('canterlot'))).toEqual([]);
    const fresh = await openDb('alice'); expect(await fresh.list('drafts')).toEqual([]);
  } finally { stop(); drafts.retire(); }
});
test('removing_the_owner_key_in_another_window_counts_as_signout', async () => {
  const { watchSignout } = await load('signout');
  const fired = vi.fn(); const stop = watchSignout(fired);
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot.theme', newValue: null }));
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot:current_owner', newValue: 'bob' }));
  expect(fired).not.toHaveBeenCalled();
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot:current_owner', oldValue: 'alice', newValue: null }));
  expect(fired).toHaveBeenCalledOnce(); stop();
});
test('owner_change_starts_clean', async () => {
  storage(); await bindOwner('alice'); const db = await openDb('alice'); await db.put('share_inbox', 'x', { text: 'private' }); localStorage.setItem('canterlot.theme', 'light');
  await bindOwner('bob'); expect(await (await openDb('alice')).list('share_inbox')).toEqual([]); expect(localStorage.getItem('canterlot.theme')).toBeNull();
});
test('csp_directives_are_complete_and_inline_scripts_are_hashed', async () => {
  const { scriptHashes, scriptsAllowed } = await load('csp');
  const path = '../../../csp.mjs'; const { directives } = await import(path);
  expect(Object.keys(directives)).toEqual(['default-src', 'script-src', 'style-src', 'img-src', 'connect-src', 'worker-src', 'manifest-src', 'object-src', 'base-uri', 'form-action', 'frame-ancestors']);
  const shell = '<script src="/theme-init.js"></script><script>start()</script>';
  const hash = `sha256-${createHash('sha256').update('start()').digest('base64')}`;
  expect(await scriptHashes(shell)).toEqual([hash]); expect(await scriptsAllowed(shell, [hash])).toBe(true); expect(await scriptsAllowed(shell, [])).toBe(false); expect(await scriptsAllowed('<button onclick="x()">x</button>', [])).toBe(false);
});
test('kit_csp_connect_src_is_self_only_and_the_header_adds_the_wss_origin', async () => {
  const path = '../../../csp.mjs'; const { directives, productionCsp } = await import(path);
  expect(directives['connect-src']).toEqual(['self']); expect(productionCsp({ websocketOrigin: 'wss://app.test' })).toContain("connect-src 'self' wss://app.test");
  expect(readFileSync('../README.md', 'utf8')).toContain('wss://<origin-host>');
});
