import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createRawSnippet, flushSync, mount, unmount } from 'svelte';
import { page } from '../stubs/app-state.svelte.js';

vi.mock('#lib/app/browser.js', () => ({ browserRuntime: () => new Promise(() => undefined) }));
vi.mock('#lib/pwa/register.js', () => ({ installation: () => undefined, register: () => new Promise(() => undefined) }));
vi.mock('#lib/pwa/signout.js', () => ({
  watchSignout: () => () => undefined,
  followSignout: async () => undefined,
  purgeLocal: async () => undefined,
  clearOwnerPreferences: () => undefined
}));
vi.mock('#lib/screens/sessions/SessionList.svelte', async () => ({ default: (await import('./support/Blank.svelte')).default }));
vi.mock('#lib/shell/Nav.svelte', async () => ({ default: (await import('./support/Blank.svelte')).default }));
vi.mock('#lib/shell/ConnectionBanner.svelte', async () => ({ default: (await import('./support/Blank.svelte')).default }));

const { default: AppShell } = await import('#lib/shell/AppShell.svelte');

describe('shell/rotation', () => {
  let instance: ReturnType<typeof mount> | undefined;
  let width = 412;

  beforeEach(() => {
    width = 412;
    Object.defineProperty(document.documentElement, 'clientWidth', { configurable: true, get: () => width });
    Object.defineProperty(document.documentElement, 'clientHeight', { configurable: true, get: () => 915 });
    vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
    page.url = new URL('http://localhost/machines');
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    document.body.textContent = '';
    vi.unstubAllGlobals();
  });

  function resize(next: number): string | null {
    width = next;
    window.dispatchEvent(new Event('resize'));
    flushSync();
    return document.querySelector('[data-layout]')?.getAttribute('data-layout') ?? null;
  }

  it('keeps the route screen and what the owner typed when the layout changes', () => {
    const children = createRawSnippet(() => ({ render: () => '<main data-screen><textarea></textarea></main>' }));
    instance = mount(AppShell, { target: document.body, props: { children } });
    flushSync();
    expect(document.querySelector('[data-layout]')?.getAttribute('data-layout')).toBe('phone');
    const screen = document.querySelector('[data-screen]')!;
    const field = document.querySelector('textarea')!;
    // Правка пузыря, диктовка и открытые листы живут в состоянии экрана: его узлы и есть это состояние.
    field.value = 'правка, которой нет в черновике';

    // Телефон повернули боком: ширина перешла порог планшета.
    expect(resize(915)).toBe('tablet');
    expect(document.querySelector('[data-screen]')).toBe(screen);
    expect(document.querySelector('textarea')).toBe(field);
    expect(field.value).toBe('правка, которой нет в черновике');
    expect(screen.closest('[data-pane="chat"]')).not.toBeNull();

    expect(resize(412)).toBe('phone');
    expect(document.querySelector('[data-screen]')).toBe(screen);
    expect(screen.closest('[data-pane]')).toBeNull();
    expect(screen.closest('.page')).not.toBeNull();
  });
});
