import type { IconName } from '#lib/ui/icons.js';

/** Значки групп: тот же набор проверяет хаб. Первый — значок по умолчанию. */
export const GROUP_ICONS = [
  { id: 'folder', label: 'Папка' },
  { id: 'briefcase', label: 'Портфель' },
  { id: 'home', label: 'Дом' },
  { id: 'star', label: 'Звезда' },
  { id: 'code', label: 'Код' },
  { id: 'rocket', label: 'Ракета' },
  { id: 'book', label: 'Книга' },
  { id: 'flask', label: 'Колба' },
  { id: 'wrench', label: 'Ключ' },
  { id: 'server', label: 'Сервер' },
  { id: 'globe', label: 'Глобус' },
  { id: 'heart', label: 'Сердце' },
  { id: 'user', label: 'Человек' },
  { id: 'users', label: 'Люди' },
  { id: 'gamepad', label: 'Игры' },
  { id: 'music', label: 'Музыка' }
] as const satisfies readonly { id: IconName; label: string }[];

export type GroupIcon = (typeof GROUP_ICONS)[number]['id'];

/** Значок группы для экрана: незнакомое или пустое значение — папка. */
export function groupIcon(value: string | null | undefined): GroupIcon {
  return GROUP_ICONS.find((icon) => icon.id === value)?.id ?? 'folder';
}
