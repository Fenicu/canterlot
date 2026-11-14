import type { components } from '#lib/api/generated/openapi.js';
import { fmt } from '#lib/format/ru.js';
import type { ViewerShot } from '#lib/viewer/shots.js';

export type GalleryItem = components['schemas']['ImageItem'];
export type GalleryPage = components['schemas']['ImagesPage'];
export type GalleryKind = GalleryItem['kind'];

export interface GalleryAnchor {
  kind: GalleryKind;
  id: string;
}

export interface GalleryQuery {
  before?: string;
  after?: string;
  around?: string;
  limit?: number;
}

/** Сколько картинок просит страница: соседей вокруг места и по краям списка. */
export const GALLERY_PAGE = 60;

export function galleryId(kind: GalleryKind, id: string): string {
  return `${kind}:${id}`;
}

/** Картинка хаба в виде кадра просмотрщика: блок отдаёт медиа, вложение и файл — своё содержимое. */
export function galleryShot(item: GalleryItem): ViewerShot {
  const id = item.media_id ?? item.attachment_id ?? '';
  return {
    media_id: id,
    src: item.media_id
      ? `/api/v1/media/${encodeURIComponent(item.media_id)}`
      : `/api/v1/attachments/${encodeURIComponent(id)}/content`,
    mime: item.mime,
    name: item.name ?? undefined,
    size: item.size,
    width: item.width ?? null,
    height: item.height ?? null,
    caption: [item.name, item.size > 0 ? fmt.bytes(item.size) : ''].filter(Boolean).join(' · ')
  };
}

/** Карточка, из которой открыли просмотрщик, уже знает подпись и имя кадра: у того же id они остаются. */
export function mergeGalleryShots(items: readonly GalleryItem[], local: readonly ViewerShot[]): ViewerShot[] {
  const known = new Map(local.map((shot) => [shot.media_id, shot]));
  return items.map((item) => known.get(item.media_id ?? item.attachment_id ?? '') ?? galleryShot(item));
}

export class GalleryMissing extends Error {
  constructor() {
    super('gallery_missing');
  }
}

/**
 * Страницы картинок разговора вокруг одной: старшие подставляются в начало,
 * младшие в конец, место считается как `offset + index`.
 */
export class Gallery {
  items: GalleryItem[] = [];
  total: number | null = null;
  /** Сколько картинок старше `items[0]`. */
  offset = 0;
  /** Курсор старших; пусто — старше ничего нет. */
  before: string | null = null;
  after: string | null = null;
  #load: (query: GalleryQuery) => Promise<GalleryPage>;
  #anchor: string;
  #started = false;
  #older = false;
  #newer = false;

  constructor(load: (query: GalleryQuery) => Promise<GalleryPage>, anchor: string) {
    this.#load = load;
    this.#anchor = anchor;
  }

  async start(): Promise<void> {
    const page = await this.#load({ around: this.#anchor });
    this.#replace(page);
    if (!this.items.some((item) => item.id === this.#anchor)) throw new GalleryMissing();
    this.#started = true;
  }

  /** Старшая страница; сколько картинок добавилось. */
  async older(): Promise<number> {
    if (!this.#started || !this.before || this.#older) return 0;
    this.#older = true;
    try {
      const page = await this.#load({ before: this.before, limit: GALLERY_PAGE });
      const seen = new Set(this.items.map((item) => item.id));
      const fresh = page.items.filter((item) => !seen.has(item.id));
      this.items = [...fresh, ...this.items];
      this.offset = page.offset;
      this.total = page.total;
      this.before = fresh.length > 0 ? (page.next_before ?? null) : null;
      return fresh.length;
    } finally {
      this.#older = false;
    }
  }

  /** Младшая страница; сколько картинок добавилось. */
  async newer(): Promise<number> {
    if (!this.#started || !this.after || this.#newer) return 0;
    this.#newer = true;
    try {
      const page = await this.#load({ after: this.after, limit: GALLERY_PAGE });
      const seen = new Set(this.items.map((item) => item.id));
      const fresh = page.items.filter((item) => !seen.has(item.id));
      this.items = [...this.items, ...fresh];
      this.total = page.total;
      this.after = fresh.length > 0 ? (page.next_after ?? null) : null;
      return fresh.length;
    } finally {
      this.#newer = false;
    }
  }

  #replace(page: GalleryPage): void {
    this.items = page.items;
    this.total = page.total;
    this.offset = page.offset;
    this.before = page.next_before ?? null;
    this.after = page.next_after ?? null;
  }
}

/** Место картинки в списке: полный номер в разговоре для счётчика «12 из 40». */
export function galleryPosition(offset: number, index: number): number {
  return offset + index + 1;
}
