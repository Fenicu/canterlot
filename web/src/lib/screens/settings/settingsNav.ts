import type { LayoutMode } from '#lib/shell/layout.js';

/** Разделы настроек по порядку; адрес раздела — `/settings?c=<id>`. */
export const SETTINGS_CATEGORIES = [
  { id: 'appearance', title: 'Внешний вид', hint: 'Режим, тема, анимации' },
  { id: 'input', title: 'Ввод и чтение', hint: 'Enter, подсказки, рассуждения' },
  { id: 'limits', title: 'Лимиты и делегирование', hint: 'Пороги, резерв, запуск без вопроса' },
  { id: 'routing', title: 'Маршрутизация', hint: 'Кого брать для каждого типа задачи' },
  { id: 'security', title: 'Вход и безопасность', hint: 'Подключение, сеансы, ключи, выход' },
  { id: 'app', title: 'Приложение', hint: 'Версии, обновления, установка' }
] as const;

export type SettingsCategory = (typeof SETTINGS_CATEGORIES)[number]['id'];

/**
 * Открытый раздел: запрошенный, если он есть; без выбора стол открывает первый раздел
 * рядом со списком, а телефон и планшет показывают сам список разделов.
 */
export function settingsCategory(requested: string | null, layout: LayoutMode): SettingsCategory | null {
  const known = SETTINGS_CATEGORIES.find((entry) => entry.id === requested);
  if (known) return known.id;
  return layout === 'desktop' ? 'appearance' : null;
}

export function categoryHref(id: SettingsCategory): `/settings?c=${SettingsCategory}` {
  return `/settings?c=${id}`;
}
