import { describe, expect, it } from 'vitest';
import type { components } from '#lib/api/generated/openapi.js';
import { Gallery, galleryId, galleryShot, mergeGalleryShots } from '#lib/viewer/gallery.js';
import type { ViewerShot } from '#lib/viewer/shots.js';

type ImageItem = components['schemas']['ImageItem'];
type ImagesPage = components['schemas']['ImagesPage'];

function item(patch: Partial<ImageItem> & Pick<ImageItem, 'kind' | 'seq'>): ImageItem {
  const id = patch.media_id ?? patch.attachment_id ?? crypto.randomUUID();
  return {
    id: patch.id ?? galleryId(patch.kind, id),
    media_id: null,
    attachment_id: null,
    name: null,
    mime: 'image/png',
    size: 2048,
    width: null,
    height: null,
    created_at: '2026-11-02T12:00:00Z',
    input_id: null,
    block_id: null,
    ...patch
  } as ImageItem;
}

function page(items: ImageItem[], patch: Partial<ImagesPage> = {}): ImagesPage {
  return { items, total: items.length, offset: 0, next_before: null, next_after: null, ...patch };
}

describe('galleryShot', () => {
  it('block media loads from the media route and attachment from the attachment route', () => {
    const media = galleryShot(item({ kind: 'block_media', seq: '10', media_id: 'm1', width: 800, height: 600 }));
    expect(media).toMatchObject({ media_id: 'm1', src: '/api/v1/media/m1', width: 800, height: 600, mime: 'image/png' });
    const file = galleryShot(item({ kind: 'block_file', seq: '20', attachment_id: 'a1', name: 'desk.png', size: 4096 }));
    expect(file).toMatchObject({
      media_id: 'a1',
      src: '/api/v1/attachments/a1/content',
      name: 'desk.png',
      size: 4096,
      caption: 'desk.png · 4 КиБ'
    });
    expect(galleryId('input_attachment', 'a2')).toBe('input_attachment:a2');
  });
});

describe('mergeGalleryShots', () => {
  it('keeps the caption and size the caller already had for the same picture', () => {
    const local: ViewerShot[] = [{ media_id: 'a1', src: '/api/v1/attachments/a1/content', name: 'shot.png', size: 10, caption: 'shot.png · 10 Б' }];
    const merged = mergeGalleryShots(
      [item({ kind: 'input_attachment', seq: '1', attachment_id: 'a1' }), item({ kind: 'block_media', seq: '2', media_id: 'm2' })],
      local
    );
    expect(merged[0]).toBe(local[0]);
    expect(merged[1]?.media_id).toBe('m2');
  });
});

describe('Gallery', () => {
  it('starts around the tapped picture and keeps total and position from the hub', async () => {
    const calls: unknown[] = [];
    const gallery = new Gallery(async (query) => {
      calls.push(query);
      return page(
        [item({ kind: 'block_media', seq: '20', media_id: 'm2' }), item({ kind: 'block_media', seq: '30', media_id: 'm3' })],
        { total: 40, offset: 12, next_before: '20:0:m2', next_after: '30:0:m3' }
      );
    }, 'block_media:m2');
    await gallery.start();
    expect(calls).toEqual([{ around: 'block_media:m2' }]);
    expect(gallery.items.map((entry) => entry.id)).toEqual(['block_media:m2', 'block_media:m3']);
    expect(gallery.total).toBe(40);
    expect(gallery.offset).toBe(12);
    expect(gallery.before).toBe('20:0:m2');
    expect(gallery.after).toBe('30:0:m3');
  });

  it('prepends older pages without duplicates and recounts the offset', async () => {
    const older = [
      item({ kind: 'input_attachment', seq: '1', attachment_id: 'a0' }),
      item({ kind: 'input_attachment', seq: '2', attachment_id: 'a1' })
    ];
    const gallery = new Gallery(async (query) => {
      if (query.around) {
        return page([item({ kind: 'block_media', seq: '10', media_id: 'm1' })], { total: 3, offset: 2, next_before: '2:0:a1' });
      }
      return page(older, { total: 3, offset: 0, next_before: null, next_after: null });
    }, 'block_media:m1');
    await gallery.start();
    expect(await gallery.older()).toBe(2);
    expect(gallery.items.map((entry) => entry.id)).toEqual([
      'input_attachment:a0',
      'input_attachment:a1',
      'block_media:m1'
    ]);
    expect(gallery.offset).toBe(0);
    expect(gallery.before).toBeNull();
    expect(await gallery.older()).toBe(0);
  });

  it('appends newer pages and stops at the end of the list', async () => {
    const gallery = new Gallery(async (query) => {
      if (query.around) {
        return page([item({ kind: 'block_media', seq: '10', media_id: 'm1' })], { total: 2, next_after: '20:0:m2' });
      }
      return page([item({ kind: 'block_media', seq: '20', media_id: 'm2' })], { total: 2, offset: 1 });
    }, 'block_media:m1');
    await gallery.start();
    expect(await gallery.newer()).toBe(1);
    expect(gallery.items.map((entry) => entry.id)).toEqual(['block_media:m1', 'block_media:m2']);
    expect(gallery.after).toBeNull();
    expect(await gallery.newer()).toBe(0);
  });

  it('does not page in the same direction twice at once', async () => {
    let calls = 0;
    let release = () => {};
    const gallery = new Gallery(async () => {
      calls += 1;
      if (calls === 1) return page([item({ kind: 'block_media', seq: '10', media_id: 'm1' })], { total: 3, next_before: '5:0:m0' });
      await new Promise<void>((resolve) => { release = resolve; });
      return page([item({ kind: 'block_media', seq: '5', media_id: 'm0' })], { total: 3, offset: 0 });
    }, 'block_media:m1');
    await gallery.start();
    const first = gallery.older();
    const second = gallery.older();
    release();
    expect(await first).toBe(1);
    expect(await second).toBe(0);
    expect(calls).toBe(2);
  });

  it('fails when the hub does not return the tapped picture', async () => {
    const gallery = new Gallery(async () => page([item({ kind: 'block_media', seq: '10', media_id: 'other' })]), 'block_media:m1');
    await expect(gallery.start()).rejects.toThrow('gallery_missing');
  });
});
