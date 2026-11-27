import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import type { ChatStore } from '#lib/stores/chat.svelte.js';
import { setPointerTypeForTest } from '#lib/composer/sendRules.js';
import { FakeComposerModel } from './support/fakeComposerModel.svelte.js';

let uploadItems: { phase: string }[] = [];
const attached: unknown[] = [];

class AttachModel extends FakeComposerModel {
  override attach(files?: FileList | File[]): void {
    if (files) attached.push(files);
  }
}

vi.mock('#lib/app/runtime.js', () => ({
  getRuntime: () => ({
    journal: {},
    drafts: {},
    deviceId: 'device-1',
    connection: { status: { phase: 'live' } },
    machines: { items: new Map() }
  })
}));
vi.mock('#lib/screens/chat/open.js', () => ({
  sessionUploads: () => ({ list: () => uploadItems, retry: vi.fn(), cancel: vi.fn() })
}));
vi.mock('#lib/api/config.js', () => ({ loadConfig: () => new Promise(() => undefined) }));
vi.mock('#lib/dictation/status.svelte.js', () => ({
  dictationStatus: { available: true, missing: false, refresh: () => Promise.resolve(), hideForMissingMicrophone: vi.fn(), noteUnavailable: vi.fn() }
}));
vi.mock('#lib/composer/model.svelte.js', async (original) => ({
  ...(await original<typeof import('#lib/composer/model.svelte.js')>()),
  ComposerModel: AttachModel
}));

const { default: Composer } = await import('#lib/composer/Composer.svelte');

function chatWith(): ChatStore {
  return {
    id: 'conv-1',
    detail: { harness: { id: 'codex', title: 'Codex', provider: 'codex' }, machine: { id: 'm1', name: 'workstation' } },
    activity: { state: 'idle', evidence: {} },
    queue: { pause_reasons: [], hold_reasons: [], run_slot: 'free', queued: 0 },
    promptSuggestion: null,
    context: { tokens: 9_600, window: 200_000, observed_at: '2026-11-25T10:00:00.000Z' },
    draft: null,
    draftSerial: 0
  } as unknown as ChatStore;
}

describe('composer/attach menu', () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
    uploadItems = [];
    attached.length = 0;
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe(): void {}
        disconnect(): void {}
      }
    );
    Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: { getUserMedia: vi.fn() } });
  });

  afterEach(async () => {
    if (instance) await unmount(instance);
    instance = undefined;
    document.body.textContent = '';
    vi.unstubAllGlobals();
    setPointerTypeForTest('unknown');
  });

  function show(): HTMLElement {
    const target = document.createElement('div');
    document.body.appendChild(target);
    instance = mount(Composer, { target, props: { chat: chatWith() } });
    flushSync();
    return target;
  }

  const attachButton = (target: HTMLElement) => target.querySelector<HTMLButtonElement>('[aria-label="Прикрепить файл"]')!;
  const menu = (target: HTMLElement) => target.querySelector<HTMLElement>('[role="menu"]');
  const item = (target: HTMLElement, label: string) =>
    [...target.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((node) => node.textContent === label)!;
  const filesInput = (target: HTMLElement) => target.querySelector<HTMLInputElement>('input.file:not([accept])')!;
  const galleryInput = (target: HTMLElement) => target.querySelector<HTMLInputElement>('input[accept="image/*"]')!;
  const clickOf = (input: HTMLInputElement) => vi.spyOn(input, 'click').mockImplementation(() => {});

  function pick(input: HTMLInputElement, name: string): void {
    const file = new File(['x'], name, { type: 'image/png' });
    Object.defineProperty(input, 'files', { configurable: true, value: [file] });
    input.dispatchEvent(new Event('change', { bubbles: true }));
    flushSync();
  }

  it('touch_attach_button_opens_a_menu_with_gallery_and_files', () => {
    setPointerTypeForTest('touch');
    const target = show();
    const button = attachButton(target);
    expect(button.getAttribute('aria-expanded')).toBe('false');
    expect(menu(target)).toBeNull();

    button.click();
    flushSync();

    expect(button.getAttribute('aria-expanded')).toBe('true');
    expect([...menu(target)!.querySelectorAll('[role="menuitem"]')].map((node) => node.textContent)).toEqual(['Галерея', 'Файлы']);
  });

  it('touch_gallery_item_clicks_the_image_only_input_and_closes_the_menu', () => {
    setPointerTypeForTest('touch');
    const target = show();
    const gallery = clickOf(galleryInput(target));
    const files = clickOf(filesInput(target));

    attachButton(target).click();
    flushSync();
    item(target, 'Галерея').click();
    flushSync();

    expect(galleryInput(target).getAttribute('accept')).toBe('image/*');
    expect(gallery).toHaveBeenCalledOnce();
    expect(files).not.toHaveBeenCalled();
    expect(menu(target)).toBeNull();
  });

  it('touch_files_item_clicks_the_unfiltered_input_and_closes_the_menu', () => {
    setPointerTypeForTest('touch');
    const target = show();
    const files = clickOf(filesInput(target));
    const gallery = clickOf(galleryInput(target));

    attachButton(target).click();
    flushSync();
    item(target, 'Файлы').click();
    flushSync();

    expect(filesInput(target).hasAttribute('accept')).toBe(false);
    expect(files).toHaveBeenCalledOnce();
    expect(gallery).not.toHaveBeenCalled();
    expect(menu(target)).toBeNull();
  });

  it('desktop_attach_button_clicks_the_file_input_directly_without_a_menu', () => {
    setPointerTypeForTest('mouse');
    const target = show();
    const files = clickOf(filesInput(target));
    const gallery = clickOf(galleryInput(target));

    attachButton(target).click();
    flushSync();

    expect(files).toHaveBeenCalledOnce();
    expect(gallery).not.toHaveBeenCalled();
    expect(menu(target)).toBeNull();
  });

  it('files_picked_from_either_input_reach_the_same_attach_path', () => {
    setPointerTypeForTest('touch');
    const target = show();

    pick(filesInput(target), 'report.pdf');
    pick(galleryInput(target), 'photo.png');

    expect(attached).toHaveLength(2);
    expect((attached[0] as File[])[0]!.name).toBe('report.pdf');
    expect((attached[1] as File[])[0]!.name).toBe('photo.png');
  });

  it('touch_menu_closes_on_escape', () => {
    setPointerTypeForTest('touch');
    const target = show();
    attachButton(target).click();
    flushSync();
    expect(menu(target)).not.toBeNull();

    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    flushSync();
    expect(menu(target)).toBeNull();
  });

  it('touch_menu_closes_on_a_press_outside', () => {
    setPointerTypeForTest('touch');
    const target = show();
    attachButton(target).click();
    flushSync();
    expect(menu(target)).not.toBeNull();

    target.querySelector('textarea')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    flushSync();
    expect(menu(target)).toBeNull();
  });
});
