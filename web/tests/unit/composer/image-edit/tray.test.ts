import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import AttachmentTray from '#lib/composer/AttachmentTray.svelte';
import { icons } from '#lib/ui/icons.js';
import type { Upload, UploadManager } from '#lib/uploads/manager.js';

const upload = (id: string, name: string, mime: string): Upload => ({
  id,
  conversationId: 'conv-1',
  name,
  mime,
  size: 4,
  phase: 'uploading',
  sent: 0
});

describe('composer/attachment tray pencil', () => {
  let instance: ReturnType<typeof mount> | undefined;

  beforeEach(() => {
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

  it('shows the pencil on an editable image only and opens the editor for that item', () => {
    const items = [upload('u-1', 'shot.png', 'image/png'), upload('u-2', 'report.pdf', 'application/pdf')];
    const onedit = vi.fn();
    instance = mount(AttachmentTray, {
      target: document.body,
      props: {
        uploads: { list: () => items } as unknown as UploadManager,
        conversationId: 'conv-1',
        canEdit: (id: string) => id === 'u-1',
        onedit
      }
    });
    flushSync();

    const rows = [...document.querySelectorAll('li')];
    const pencil = (row: Element) => row.querySelector<HTMLButtonElement>('button[aria-label="Редактировать изображение"]');
    expect(pencil(rows[0]!)).not.toBeNull();
    expect(pencil(rows[0]!)!.querySelector('path')?.getAttribute('d')).toBe(icons.edit);
    expect(pencil(rows[1]!)).toBeNull();

    pencil(rows[0]!)!.click();
    expect(onedit).toHaveBeenCalledWith('u-1');
  });
});
