import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { icons } from '#lib/ui/icons.js';

vi.mock('#lib/composer/imageEdit/decode.js', () => ({
  loadImage: () => Promise.resolve(null)
}));

const { default: ImageEditor } = await import('#lib/composer/ImageEditor.svelte');

const button = (label: string): HTMLButtonElement =>
  [...document.querySelectorAll<HTMLButtonElement>('dialog.editor .tools > button')]
    .find((node) => node.getAttribute('aria-label') === label)!;

const click = (node: HTMLElement): void => {
  node.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  flushSync();
};

describe('composer/image editor toolbar', () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
    HTMLDialogElement.prototype.showModal = function () { this.setAttribute('open', ''); };
    HTMLDialogElement.prototype.close = function () { this.removeAttribute('open'); };
    HTMLCanvasElement.prototype.getContext = () => null;
    vi.stubGlobal('ResizeObserver', class {
      observe(): void {}
      disconnect(): void {}
    });
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    document.body.textContent = '';
    vi.unstubAllGlobals();
  });

  function show(): void {
    instance = mount(ImageEditor, {
      target: document.body,
      props: {
        file: new File(['x'], 'shot.png', { type: 'image/png' }),
        ondone: vi.fn(),
        oncancel: vi.fn()
      }
    });
    flushSync();
  }

  it('shows icon tools with Russian tooltips and disables undo on an empty history', () => {
    show();
    const crop = button('Обрезать');
    const pencil = button('Карандаш');
    const undo = button('Отменить действие');

    expect(crop.getAttribute('title')).toBe('Обрезать');
    expect(crop.querySelector('path')?.getAttribute('d')).toBe(icons.crop);
    expect(crop.getAttribute('aria-pressed')).toBe('true');
    expect(pencil.getAttribute('title')).toBe('Карандаш');
    expect(pencil.getAttribute('aria-pressed')).toBe('false');
    expect(undo.getAttribute('title')).toBe('Отменить действие');
    expect(undo.querySelector('path')?.getAttribute('d')).toBe(icons.undo);
    expect(undo.getAttribute('aria-disabled')).toBe('true');
    expect([...document.querySelectorAll('dialog.editor footer button')].map((node) => node.textContent?.trim())).toEqual([
      'Отмена',
      'Готово'
    ]);
  });

  it('moves the pressed state to the pencil when a colour is chosen', () => {
    show();
    click(button('Карандаш'));
    expect(button('Обрезать').getAttribute('aria-pressed')).toBe('false');
    expect(button('Карандаш').getAttribute('aria-pressed')).toBe('true');

    const green = document.querySelector<HTMLButtonElement>('dialog.editor .palette button.green')!;
    click(green);
    expect(green.getAttribute('aria-pressed')).toBe('true');
    expect(button('Карандаш').getAttribute('aria-pressed')).toBe('true');
  });
});
