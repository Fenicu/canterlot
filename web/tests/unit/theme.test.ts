import { test, expect, vi, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { PALETTES, DEFAULT_PALETTE } from '../../src/lib/palettes';

function match(query: string) {
  return { media: query, matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(), onchange: null, dispatchEvent: vi.fn(), addListener: vi.fn(), removeListener: vi.fn() };
}

const media = new Map<string, ReturnType<typeof match>>();

function setup() {
  media.clear();
  vi.stubGlobal('matchMedia', (query: string) => {
    if (!media.has(query)) media.set(query, match(query));
    return media.get(query)!;
  });
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { fn(0); return 0; });
}

async function load() {
  vi.resetModules();
  const module = await import('../../src/lib/theme.svelte');
  return { ...module, release: module.initTheme() };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  media.clear();
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
  document.documentElement.removeAttribute('data-palette');
  document.documentElement.removeAttribute('data-theme-switching');
  document.documentElement.removeAttribute('data-theme-reveal');
  Reflect.deleteProperty(document, 'startViewTransition');
});

test('stored_mode_keeps_working_and_defaults_to_standard', async () => {
  setup();
  localStorage.setItem('canterlot.theme', 'light');
  const { theme, release } = await load();
  expect(theme.pref).toBe('light');
  expect(theme.effective).toBe('light');
  expect(theme.palette).toBe('standard');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.dataset.palette).toBe('standard');
  expect(localStorage.getItem('canterlot.theme')).toBe('light');
  release();
});

test('unknown_stored_values_fall_back_to_dark_standard', async () => {
  setup();
  localStorage.setItem('canterlot.theme', 'sepia');
  localStorage.setItem('canterlot.palette', 'solarized-x');
  const { theme, release } = await load();
  expect(theme.pref).toBe('dark');
  expect(theme.palette).toBe('standard');
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(document.documentElement.dataset.palette).toBe('standard');
  release();
});

test('palette_is_stored_under_its_own_key_and_applied', async () => {
  setup();
  const { theme, release } = await load();
  theme.setPalette('standard');
  expect(theme.palette).toBe('standard');
  expect(localStorage.getItem('canterlot.palette')).toBe('standard');
  expect(document.documentElement.dataset.palette).toBe('standard');
  theme.setPalette('vscode');
  expect(localStorage.getItem('canterlot.palette')).toBe('vscode');
  expect(document.documentElement.dataset.palette).toBe('vscode');
  release();
});

test('system_mode_follows_match_media_inside_the_chosen_palette', async () => {
  setup();
  const { theme, release } = await load();
  theme.setPalette('vscode');
  theme.set('system');
  expect(theme.effective).toBe('dark');
  expect(document.documentElement.dataset.palette).toBe('vscode');
  expect(document.documentElement.dataset.theme).toBe('dark');
  const query = media.get('(prefers-color-scheme: light)')!;
  query.matches = true;
  const listener = query.addEventListener.mock.calls[0]?.[1] as () => void;
  listener();
  expect(theme.effective).toBe('light');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.dataset.palette).toBe('vscode');
  release();
});

test('toggle_flips_the_mode_and_keeps_the_palette', async () => {
  setup();
  const { theme, release } = await load();
  theme.setPalette('standard');
  theme.set('dark');
  theme.toggle();
  expect(theme.pref).toBe('light');
  expect(theme.effective).toBe('light');
  expect(theme.palette).toBe('standard');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.dataset.palette).toBe('standard');
  expect(localStorage.getItem('canterlot.palette')).toBe('standard');
  release();
});

test('storage_events_sync_both_mode_and_palette', async () => {
  setup();
  const { theme, release } = await load();
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot.palette', newValue: 'vscode' }));
  expect(theme.palette).toBe('vscode');
  expect(document.documentElement.dataset.palette).toBe('vscode');
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot.theme', newValue: 'light' }));
  expect(theme.pref).toBe('light');
  expect(theme.effective).toBe('light');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.dataset.palette).toBe('vscode');
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot.palette', newValue: 'unknown' }));
  expect(theme.palette).toBe('standard');
  expect(document.documentElement.dataset.palette).toBe('standard');
  release();
});

test('existing_stored_palette_is_kept', async () => {
  setup();
  localStorage.setItem('canterlot.palette', 'aquamarine');
  const { theme, release } = await load();
  expect(theme.palette).toBe('aquamarine');
  expect(document.documentElement.dataset.palette).toBe('aquamarine');
  release();
});

test('every_palette_is_accepted_and_the_default_is_standard', async () => {
  setup();
  expect(DEFAULT_PALETTE).toBe('standard');
  const { theme, release } = await load();
  for (const { key } of PALETTES) {
    theme.setPalette(key);
    expect(document.documentElement.dataset.palette).toBe(key);
    expect(localStorage.getItem('canterlot.palette')).toBe(key);
  }
  release();
});

function prepaint(stored: Record<string, string>) {
  const html = { dataset: {} as Record<string, string> };
  runInNewContext(readFileSync('static/theme-init.js', 'utf8'), {
    localStorage: { getItem: (key: string) => stored[key] ?? null },
    matchMedia: () => ({ matches: false }),
    document: { documentElement: html }
  });
  return html.dataset;
}

test('prepaint_script_knows_every_palette_and_the_same_default', () => {
  const source = readFileSync('static/theme-init.js', 'utf8');
  const list = JSON.parse(source.match(/const palettes = (\[[^\]]*\])/)![1]!.replaceAll("'", '"')) as string[];
  expect(list).toEqual(PALETTES.map(p => p.key));
  expect(prepaint({})).toEqual({ palette: DEFAULT_PALETTE, theme: 'dark' });
  expect(prepaint({ 'canterlot.palette': 'unknown' }).palette).toBe(DEFAULT_PALETTE);
  for (const { key } of PALETTES) expect(prepaint({ 'canterlot.palette': key, 'canterlot.theme': 'light' })).toEqual({ palette: key, theme: 'light' });
});

function fakeTransition() {
  const calls: (() => void)[] = [];
  const start = vi.fn((update: () => void) => {
    calls.push(update);
    update();
    return { ready: Promise.resolve(), finished: Promise.resolve(), updateCallbackDone: Promise.resolve(), skipTransition: vi.fn() };
  });
  Object.defineProperty(document, 'startViewTransition', { value: start, configurable: true, writable: true });
  return start;
}

test('owner_choice_reveals_through_a_view_transition', async () => {
  setup();
  const { theme, release } = await load();
  const start = fakeTransition();
  const animate = vi.fn();
  document.documentElement.animate = animate as unknown as typeof document.documentElement.animate;
  theme.setPalette('forest');
  expect(start).toHaveBeenCalledTimes(1);
  expect(document.documentElement.dataset.palette).toBe('forest');
  await Promise.resolve(); await Promise.resolve();
  expect(animate).toHaveBeenCalledTimes(1);
  expect(animate.mock.calls[0]![1]).toMatchObject({ pseudoElement: '::view-transition-new(root)' });
  theme.toggle();
  expect(start).toHaveBeenCalledTimes(2);
  expect(document.documentElement.dataset.theme).toBe('light');
  release();
});

test('reduced_motion_motion_off_and_sync_switch_instantly', async () => {
  setup();
  const { theme, release } = await load();
  const start = fakeTransition();
  const reduced = (window.matchMedia('(prefers-reduced-motion: reduce)'), media.get('(prefers-reduced-motion: reduce)')!);
  reduced.matches = true;
  theme.setPalette('sepia');
  expect(document.documentElement.dataset.palette).toBe('sepia');
  reduced.matches = false;
  document.documentElement.style.setProperty('--motion-scale', '0');
  theme.setPalette('nord');
  expect(document.documentElement.dataset.palette).toBe('nord');
  document.documentElement.style.removeProperty('--motion-scale');
  window.dispatchEvent(new StorageEvent('storage', { key: 'canterlot.palette', newValue: 'lavender' }));
  expect(document.documentElement.dataset.palette).toBe('lavender');
  expect(start).not.toHaveBeenCalled();
  release();
});

test('unchanged_choice_does_not_start_a_transition', async () => {
  setup();
  const { theme, release } = await load();
  const start = fakeTransition();
  theme.setPalette('standard');
  theme.set('dark');
  expect(start).not.toHaveBeenCalled();
  release();
});
