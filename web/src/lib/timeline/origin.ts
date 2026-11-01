import { modelLabel } from '#lib/format/labels.js';
import type { InputOrigin } from '#lib/stores/inputs.js';

/** Подпись над вводом от другой сессии; ввод владельца подписи не имеет. */
export function originLabel(origin: InputOrigin | null | undefined): string | null {
  if (!origin) return null;
  const title = origin.title?.trim();
  if (origin.kind === 'event') {
    if (title) return `Итог сессии ${title}`;
    return origin.sender_deleted ? 'Итог удалённой сессии' : 'Сообщение хаба';
  }
  if (!title) return origin.sender_deleted ? 'От удалённой сессии' : 'От другой сессии';
  const model = origin.model?.trim();
  return model ? `От сессии ${title} · модель ${modelLabel(model)}` : `От сессии ${title}`;
}
