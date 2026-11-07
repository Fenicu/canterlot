import { THUMB_MAX_BYTES } from '#lib/timeline/attachments.js';
import type { Size } from '#lib/viewer/zoom.js';

/** Картинка просмотрщика: медиа блока ответа или вложение. */
export interface ViewerShot {
  media_id: string;
  /** Адрес, если картинка не из блока ответа: например вложение сообщения. */
  src?: string;
  mime?: string;
  width?: number | null;
  height?: number | null;
  /** Подпись под картинкой вместо размеров. */
  caption?: string;
  /** Имя файла для «Сохранить»; без него имя собирается из формата. */
  name?: string;
  /** Байты файла, если известны: тяжёлую соседнюю картинку заранее не тянем. */
  size?: number;
}

/** Соседей заранее грузим, только если они не тяжелее миниатюры в ленте: телефон раскодирует их целиком. */
export function preloads(shot: ViewerShot): boolean {
  return !shot.size || shot.size <= THUMB_MAX_BYTES;
}

export function shotSrc(shot: ViewerShot): string {
  return shot.src ?? `/api/v1/media/${encodeURIComponent(shot.media_id)}`;
}

export function shotFormat(shot: ViewerShot): string {
  return (shot.mime ?? '').split('/')[1]?.split(/[;+]/)[0]?.toLocaleUpperCase('ru-RU') ?? '';
}

/** Размер из описания картинки, если он известен заранее. */
export function declaredSize(shot: ViewerShot): Size | null {
  return shot.width && shot.height && shot.width > 0 && shot.height > 0 ? { width: shot.width, height: shot.height } : null;
}

/** Строка под картинкой: своя подпись или размер с форматом. */
export function shotCaption(shot: ViewerShot, natural: Size | null): string {
  if (shot.caption) return shot.caption;
  const format = shotFormat(shot);
  const size = natural ? `${natural.width} × ${natural.height}` : '';
  return [size, format].filter(Boolean).join(' · ');
}

/** Имя сохранённого файла: у вложения своё, у медиа ответа сервер отдаёт картинку без имени. */
export function downloadName(shot: ViewerShot): string {
  if (shot.name) return shot.name;
  const format = shotFormat(shot).toLowerCase();
  const extension = format === 'jpeg' ? 'jpg' : format || 'png';
  return `картинка-${shot.media_id.slice(0, 8)}.${extension}`;
}
