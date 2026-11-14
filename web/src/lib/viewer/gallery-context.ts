import { getContext, setContext } from 'svelte';
import type { GalleryItem, GalleryPage, GalleryQuery } from '#lib/viewer/gallery.js';

/** Источник картинок всей сессии и переход из просмотрщика к месту картинки. */
export interface GallerySource {
  page(query: GalleryQuery): Promise<GalleryPage>;
  /** Закрыть просмотрщик и показать место картинки в чате. */
  reveal(item: GalleryItem): void;
}

const KEY = Symbol('gallery-source');

export function setGallerySource(source: GallerySource): void {
  setContext(KEY, source);
}

/** Вне ленты (экспорт картинкой, тесты компонентов) просмотрщик листает только свои кадры. */
export function gallerySource(): GallerySource | null {
  return getContext<GallerySource | undefined>(KEY) ?? null;
}
