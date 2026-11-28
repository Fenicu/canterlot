import { describe, expect, it, vi } from 'vitest';
import { ComposerModel, type ComposerDeps } from '#lib/composer/model.svelte.js';
import type { Size } from '#lib/composer/imageEdit/model.js';
import type { Upload, UploadManager } from '#lib/uploads/manager.js';

function file(name: string, type: string): File {
  return new File(['mock'], name, { type });
}

/** Лоток без сети: add и replace меняют только список, как это видит композер. */
function harness(options: {
  decode?: (file: File) => Promise<Size | null>;
  edit?: (file: File) => Promise<File | null>;
} = {}) {
  const items: Upload[] = [];
  let next = 0;
  const view = (source: File): Upload => ({
    id: `u-${++next}`,
    conversationId: 'conv-1',
    name: source.name,
    mime: source.type,
    size: source.size,
    phase: 'uploading',
    sent: 0
  });
  const uploads = {
    list: vi.fn(() => items),
    add: vi.fn((_conversation: string, source: File) => {
      const item = view(source);
      items.push(item);
      return item;
    }),
    replace: vi.fn((id: string, source: File) => {
      const item = view(source);
      items.splice(items.findIndex((entry) => entry.id === id), 1, item);
      return item;
    })
  };
  const edit = vi.fn(options.edit ?? (async () => null));
  const deps = {
    conversationId: 'conv-1',
    uploads: uploads as unknown as UploadManager,
    config: () => ({}),
    decodeImage: options.decode ?? (async () => ({ width: 100, height: 100 })),
    editImage: edit
  } as unknown as ComposerDeps;
  return { model: new ComposerModel(deps), uploads, items, edit };
}

describe('composer/attach with the pencil editor', () => {
  it('uploads an attached image at once without opening the editor', () => {
    const { model, uploads, edit } = harness();
    const shot = file('shot.png', 'image/png');
    model.attach([shot]);
    expect(uploads.add).toHaveBeenCalledWith('conv-1', shot, 0);
    expect(edit).not.toHaveBeenCalled();
  });

  it('marks PNG, JPEG and WebP up to 16 MP as editable, other files and bigger images not', async () => {
    const sizes = new Map<string, Size>([['huge.png', { width: 4096, height: 4097 }]]);
    const { model, items } = harness({
      decode: async (source) => sizes.get(source.name) ?? { width: 4096, height: 4096 }
    });
    model.attach([
      file('a.png', 'image/png'),
      file('b.jpg', 'image/jpeg'),
      file('c.webp', 'image/webp'),
      file('d.gif', 'image/gif'),
      file('e.pdf', 'application/pdf'),
      file('huge.png', 'image/png')
    ]);
    await vi.waitFor(() => expect(model.canEdit(items[2]!.id)).toBe(true));
    expect(items.map((item) => model.canEdit(item.id))).toEqual([true, true, true, false, false, false]);
  });

  it('gives no pencil to an image that does not decode', async () => {
    const { model, items } = harness({ decode: async () => null });
    model.attach([file('broken.png', 'image/png')]);
    await Promise.resolve();
    await Promise.resolve();
    expect(model.canEdit(items[0]!.id)).toBe(false);
  });

  it('opens the editor with the attached file', async () => {
    const { model, items, edit } = harness();
    const shot = file('shot.png', 'image/png');
    model.attach([shot]);
    await vi.waitFor(() => expect(model.canEdit(items[0]!.id)).toBe(true));
    await model.edit(items[0]!.id);
    expect(edit).toHaveBeenCalledWith(shot);
  });

  it('replaces the attachment in place on «Готово» and edits the edited file next time', async () => {
    const edited = file('shot.png', 'image/png');
    const { model, items, uploads, edit } = harness({ edit: async () => edited });
    model.attach([file('first.pdf', 'application/pdf'), file('shot.png', 'image/png'), file('last.pdf', 'application/pdf')]);
    const old = items[1]!.id;
    await vi.waitFor(() => expect(model.canEdit(old)).toBe(true));
    await model.edit(old);
    expect(uploads.replace).toHaveBeenCalledWith(old, edited);
    expect(items.map((item) => item.name)).toEqual(['first.pdf', 'shot.png', 'last.pdf']);
    const fresh = items[1]!.id;
    expect(fresh).not.toBe(old);
    expect(model.canEdit(old)).toBe(false);
    expect(model.canEdit(fresh)).toBe(true);
    await model.edit(fresh);
    expect(edit).toHaveBeenLastCalledWith(edited);
  });

  it('keeps the attachment as is on «Отмена»', async () => {
    const { model, items, uploads } = harness({ edit: async () => null });
    model.attach([file('shot.png', 'image/png')]);
    const id = items[0]!.id;
    await vi.waitFor(() => expect(model.canEdit(id)).toBe(true));
    await model.edit(id);
    expect(uploads.replace).not.toHaveBeenCalled();
    expect(items.map((item) => item.id)).toEqual([id]);
    expect(model.canEdit(id)).toBe(true);
  });
});
