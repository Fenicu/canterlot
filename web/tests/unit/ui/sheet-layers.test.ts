import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

import { page } from '../stubs/app-state.svelte.js';
import { followHistory } from '../stubs/app-navigation.js';
import NestedSheets from './support/NestedSheets.svelte';
import { sheetHost } from './support/sheetHost.svelte.js';

const openDialogs = () => [...document.querySelectorAll<HTMLDialogElement>('dialog')].filter((node) => node.hasAttribute('open'));
const button = (name: string) => {
  const found = openDialogs().flatMap((node) => [...node.querySelectorAll('button')]).filter((node) => node.textContent?.trim() === name || node.getAttribute('aria-label') === name).at(-1);
  if (!found) throw new Error(`no button «${name}» in an open sheet`);
  return found;
};
const click = (name: string) => { button(name).click(); flushSync(); };

describe('ui/sheet layers', () => {
  let instance: ReturnType<typeof mount> | undefined;
  let host: ReturnType<typeof sheetHost>;
  let shell: HTMLElement;
  let unfollow: () => void;

  beforeEach(() => {
    // В jsdom у <dialog> нет модального режима и анимаций: хватает признака `open`.
    HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
    HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); };
    Element.prototype.getAnimations = () => [];
    shell = document.createElement('div');
    shell.id = 'app-shell';
    const target = document.createElement('div');
    document.body.replaceChildren(shell, target);
    unfollow = followHistory();
    page.state = {};
    host = sheetHost();
    instance = mount(NestedSheets, { target, props: { host } });
    flushSync();
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    unfollow();
    history.replaceState(null, '');
  });

  async function openDetails(): Promise<void> {
    host.details = true;
    flushSync();
    await vi.waitFor(() => expect(page.state.sheet).toBe('session-details'));
  }

  async function openConfirm(): Promise<string> {
    host.confirm = true;
    flushSync();
    await vi.waitFor(() => expect(page.state.sheet).not.toBe('session-details'));
    await vi.waitFor(() => expect(openDialogs().at(-1)?.textContent).toContain('Завершить сеанс?'));
    return page.state.sheet!;
  }

  it('keeps the lower sheet open and the shell locked while a confirmation covers it', async () => {
    await openDetails();
    await openConfirm();

    expect(openDialogs()).toHaveLength(2);
    expect(shell.inert).toBe(true);
    expect(host.log).toEqual([]);
  });

  it('unlocks the shell once the confirmation and the sheet under it are gone', async () => {
    await openDetails();
    await openConfirm();

    click('Отмена');
    expect(openDialogs()).toHaveLength(1);
    expect(shell.inert).toBe(true);

    click('Закрыть');
    expect(openDialogs()).toHaveLength(0);
    expect(shell.inert).toBe(false);
  });

  it('unlocks the shell when both layers go at once', async () => {
    await openDetails();
    await openConfirm();

    click('Завершить');

    await vi.waitFor(() => expect(openDialogs()).toHaveLength(0));
    expect(shell.inert).toBe(false);
  });
});
