/**
 * Память о виденной версии. Ошибка здесь стоит доверия: либо человек не
 * узнаёт про изменения, либо ловит окно на каждом заходе.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { APP_VERSION, SEEN_VERSION_KEY, whatsNew } from '../../src/lib/stores/appVersion';

function state(): { entries: unknown[]; open: boolean } {
  let value: { entries: unknown[]; open: boolean } = { entries: [], open: false };
  whatsNew.subscribe((next) => (value = next))();
  return value;
}

beforeEach(() => {
  localStorage.clear();
  whatsNew.dismiss();
});

describe('память о виденной версии', () => {
  it('первый вход не показывает окно, но версию запоминает', async () => {
    await whatsNew.check();

    expect(state().open).toBe(false);
    expect(localStorage.getItem(SEEN_VERSION_KEY)).toBe(APP_VERSION);
  });

  it('повторный вход на той же версии молчит', async () => {
    await whatsNew.check();
    await whatsNew.check();

    expect(state().open).toBe(false);
  });

  it('после обновления показывает пропущенное', async () => {
    localStorage.setItem(SEEN_VERSION_KEY, '0.9.0');

    await whatsNew.check();

    expect(state().open).toBe(true);
    expect(state().entries.length).toBeGreaterThan(0);
  });

  it('версия отмечается виденной сразу — окно не вернётся после перезагрузки', async () => {
    localStorage.setItem(SEEN_VERSION_KEY, '0.9.0');
    await whatsNew.check();

    expect(localStorage.getItem(SEEN_VERSION_KEY)).toBe(APP_VERSION);

    await whatsNew.check();

    expect(state().open).toBe(false);
  });

  it('заблокированное хранилище не роняет приложение', async () => {
    const boom = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('SecurityError');
    });

    await expect(whatsNew.check()).resolves.toBeUndefined();
    boom.mockRestore();
  });
});
