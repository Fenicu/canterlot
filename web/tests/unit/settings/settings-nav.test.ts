import { describe, expect, it } from 'vitest';
import { SETTINGS_CATEGORIES, categoryHref, settingsCategory } from '#lib/screens/settings/settingsNav.js';

describe('settingsCategory', () => {
  it('opens the requested category on any layout', () => {
    expect(settingsCategory('routing', 'desktop')).toBe('routing');
    expect(settingsCategory('security', 'phone')).toBe('security');
  });

  it('opens appearance on the desktop and the category list on the phone without a choice', () => {
    expect(settingsCategory(null, 'desktop')).toBe('appearance');
    expect(settingsCategory(null, 'phone')).toBeNull();
    expect(settingsCategory(null, 'tablet')).toBeNull();
  });

  it('treats an unknown category as no choice', () => {
    expect(settingsCategory('nope', 'desktop')).toBe('appearance');
    expect(settingsCategory('nope', 'phone')).toBeNull();
  });
});

describe('SETTINGS_CATEGORIES', () => {
  it('lists the six sections in order with their addresses', () => {
    expect(SETTINGS_CATEGORIES.map((entry) => entry.title)).toEqual(['Внешний вид', 'Ввод и чтение', 'Лимиты и делегирование', 'Маршрутизация', 'Вход и безопасность', 'Приложение']);
    expect(categoryHref('input')).toBe('/settings?c=input');
  });
});
