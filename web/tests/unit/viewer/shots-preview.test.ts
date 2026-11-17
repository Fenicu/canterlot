import { describe, expect, it } from 'vitest';
import { attachmentPreviewUrl } from '#lib/timeline/attachments.js';
import { shotPreviewSrc, shotSrc } from '#lib/viewer/shots.js';

describe('shotPreviewSrc', () => {
  it('строит показ медиа по media_id', () => {
    expect(shotPreviewSrc({ media_id: 'm1' })).toBe('/api/v1/media/m1/preview');
  });

  it('меняет content вложения на preview', () => {
    expect(shotPreviewSrc({ media_id: 'a1', src: '/api/v1/attachments/a1/content' })).toBe(
      '/api/v1/attachments/a1/preview'
    );
  });

  it('добавляет preview к адресу медиа из галереи', () => {
    expect(shotPreviewSrc({ media_id: 'm2', src: '/api/v1/media/m2' })).toBe(
      '/api/v1/media/m2/preview'
    );
  });

  it('оставляет чужой адрес как есть', () => {
    expect(shotPreviewSrc({ media_id: 'x', src: 'https://example.test/full.png' })).toBe(
      'https://example.test/full.png'
    );
  });
});

describe('shotSrc', () => {
  it('по-прежнему отдаёт оригинал для полного вида и скачивания', () => {
    expect(shotSrc({ media_id: 'm1' })).toBe('/api/v1/media/m1');
    expect(shotSrc({ media_id: 'a1', src: '/api/v1/attachments/a1/content' })).toBe(
      '/api/v1/attachments/a1/content'
    );
  });
});

describe('attachmentPreviewUrl', () => {
  it('строит показ вложения', () => {
    expect(attachmentPreviewUrl('a1')).toBe('/api/v1/attachments/a1/preview');
  });
});
