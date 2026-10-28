import { test, expect, vi, afterEach } from 'vitest';
import { announce, mountAnnouncements } from '../../src/lib/a11y/announce';
import { durationMs, setMotionScale, prefersReducedMotion } from '../../src/lib/motion';
const media = new Map<string, ReturnType<typeof match>>();
function match(query: string) {
  return { media: query, matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn(), onchange: null, dispatchEvent: vi.fn(), addListener: vi.fn(), removeListener: vi.fn() };
}
function setup() {
  vi.stubGlobal('matchMedia', (query: string) => { if (!media.has(query)) media.set(query, match(query)); return media.get(query)!; });
  vi.stubGlobal('requestAnimationFrame', (fn: FrameRequestCallback) => { fn(0); return 0; });
}
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); media.clear(); document.documentElement.removeAttribute('style'); document.documentElement.removeAttribute('data-motion'); localStorage.clear(); });
test('announce_collapses_identical_messages_within_two_seconds', () => {
  vi.useFakeTimers();
  const node = document.createElement('div'); const action = mountAnnouncements(node);
  announce('Подключено'); vi.runOnlyPendingTimers(); expect(node.textContent).toBe('Подключено');
  node.textContent = 'Без изменений'; announce('Подключено'); vi.runOnlyPendingTimers(); expect(node.textContent).toBe('Без изменений');
  vi.advanceTimersByTime(2000); announce('Подключено', 'assertive'); vi.runOnlyPendingTimers(); expect(node.textContent).toBe('Подключено'); expect(node.getAttribute('aria-live')).toBe('assertive');
  action.destroy();
});
test('announce_delivers_distinct_messages_in_order_and_collapses_repeats_within_window', () => {
  vi.useFakeTimers();
  const node = document.createElement('div');
  const action = mountAnnouncements(node);

  announce('Первое');
  announce('Второе');
  vi.runOnlyPendingTimers();
  expect(node.textContent).toBe('Первое');
  vi.runOnlyPendingTimers();
  expect(node.textContent).toBe('Второе');

  announce('Первое');
  vi.runOnlyPendingTimers();
  expect(node.textContent).toBe('Второе');

  vi.advanceTimersByTime(2000);
  announce('Первое');
  vi.runOnlyPendingTimers();
  expect(node.textContent).toBe('Первое');

  action.destroy();
});
test('duration_ms_reads_the_computed_token', () => {
  setup();
  vi.stubGlobal('getComputedStyle', (node: Element) => ({ transitionDuration: (node as HTMLElement).style.transitionDuration === 'var(--duration-fast)' ? '.16s' : '240ms', getPropertyValue: () => '1' }));
  expect(durationMs('--duration-fast')).toBe(160); expect(durationMs('--duration-expand')).toBe(240);
  media.get('(prefers-reduced-motion: reduce)')!.matches = true;
  expect(durationMs('--duration-fast')).toBe(0);
});
test('theme_store_persists_and_resolves_system', async () => {
  setup();
  const { initTheme, theme } = await import('../../src/lib/theme.svelte');
  const release = initTheme();
  theme.set('light'); expect(theme.pref).toBe('light'); expect(localStorage.getItem('canterlot.theme')).toBe('light'); expect(document.documentElement.dataset.theme).toBe('light');
  theme.set('system'); expect(theme.effective).toBe('dark');
  const query = media.get('(prefers-color-scheme: light)')!;
  query.matches = true;
  const listener = query.addEventListener.mock.calls[0]?.[1] as () => void; listener();
  expect(theme.effective).toBe('light'); theme.toggle(); expect(theme.pref).toBe('dark'); release();
});
test('motion_scale_off_is_static', () => {
  setup(); setMotionScale(0); expect(prefersReducedMotion()).toBe(true);
  setMotionScale(1.5); expect(document.documentElement.style.getPropertyValue('--motion-scale')).toBe('1.5');
});
