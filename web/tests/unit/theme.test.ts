import { test, expect, vi, afterEach } from 'vitest';

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
});

test('stored_mode_keeps_working_and_defaults_to_aquamarine', async () => {
  setup();
  localStorage.setItem('canterlot.theme', 'light');
  const { theme, release } = await load();
  expect(theme.pref).toBe('light');
  expect(theme.effective).toBe('light');
  expect(theme.palette).toBe('aquamarine');
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(document.documentElement.dataset.palette).toBe('aquamarine');
  expect(localStorage.getItem('canterlot.theme')).toBe('light');
  release();
});

test('unknown_stored_values_fall_back_to_dark_aquamarine', async () => {
  setup();
  localStorage.setItem('canterlot.theme', 'sepia');
  localStorage.setItem('canterlot.palette', 'solarized');
  const { theme, release } = await load();
  expect(theme.pref).toBe('dark');
  expect(theme.palette).toBe('aquamarine');
  expect(document.documentElement.dataset.theme).toBe('dark');
  expect(document.documentElement.dataset.palette).toBe('aquamarine');
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
  expect(theme.palette).toBe('aquamarine');
  expect(document.documentElement.dataset.palette).toBe('aquamarine');
  release();
});
