import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
import { followHistory } from '../stubs/app-navigation.js';

const opened = () => document.querySelectorAll('dialog[open]').length;
const click = (name: string) => {
  [...document.querySelectorAll('dialog[open] button')].find((node) => node.textContent?.trim() === name)!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  flushSync();
};

describe('ui/confirm dialog kept in the document by its host', () => {
  let instance: ReturnType<typeof mount> | undefined;
  let unfollow: () => void;

  beforeEach(() => {
    unfollow = followHistory();
    // В jsdom у <dialog> нет модального режима и анимаций: хватает признака `open`.
    HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
    HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); };
    Element.prototype.getAnimations = () => [];
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    unfollow();
  });

  it('opens again after it was cancelled and after it was confirmed', async () => {
    const onconfirm = vi.fn();
    const props = $state({ title: 'Удалить заготовку?', body: 'Заготовка будет удалена', confirmLabel: 'Удалить', open: false, onconfirm, onclose: () => { props.open = false; } });
    instance = mount(ConfirmDialog, { target: document.body, props });
    flushSync();

    props.open = true;
    flushSync();
    expect(opened()).toBe(1);
    click('Отмена');
    await vi.waitFor(() => expect(opened()).toBe(0));

    props.open = true;
    flushSync();
    expect(opened()).toBe(1);
    click('Удалить');
    await vi.waitFor(() => expect(onconfirm).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(props.open).toBe(false));

    props.open = true;
    flushSync();
    expect(opened()).toBe(1);
  });
});
