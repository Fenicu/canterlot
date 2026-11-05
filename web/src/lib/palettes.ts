/** Палитры в порядке выбора в настройках. Список повторяют блоки `tokens.css` и `static/theme-init.js`; тест сверяет все три. */
export const PALETTES = [
  { key: 'standard', name: 'Стандартная' },
  { key: 'aquamarine', name: 'Аквамарин' },
  { key: 'vscode', name: 'VS Code' },
  { key: 'ink', name: 'Чернила' },
  { key: 'graphite', name: 'Графит' },
  { key: 'slate', name: 'Сланец' },
  { key: 'sepia', name: 'Сепия' },
  { key: 'terracotta', name: 'Терракота' },
  { key: 'forest', name: 'Лес' },
  { key: 'midnight', name: 'Полночь' },
  { key: 'lavender', name: 'Лаванда' },
  { key: 'strawberry', name: 'Клубничное молоко' },
  { key: 'lemonade', name: 'Лимонад' },
  { key: 'nord', name: 'Норд' },
  { key: 'high-contrast', name: 'Высокий контраст' }
] as const;
export type PalettePref = typeof PALETTES[number]['key'];
export const DEFAULT_PALETTE: PalettePref = 'standard';
const KEYS = new Set<string>(PALETTES.map(palette => palette.key));
/** Неизвестное или пустое значение хранилища получает палитру по умолчанию. */
export function paletteOrDefault(value: unknown): PalettePref {
  return typeof value === 'string' && KEYS.has(value) ? value as PalettePref : DEFAULT_PALETTE;
}
