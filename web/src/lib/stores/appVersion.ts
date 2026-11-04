/**
 * Какую версию сервиса человек уже видел.
 *
 * Хранится в браузере: сервер про «прочитал ли ты, что нового» ничего не
 * знает, да и знать не должен — это свойство устройства, а не учётной записи.
 */
import { writable } from "svelte/store";

import type { ChangelogEntry } from "#lib/changelog.js";
import { compareVersions } from "#lib/version.js";

export const SEEN_VERSION_KEY = "canterlot.seen-version";

/** Версия из `[workspace.package]`, подставленная сборкой. */
export const APP_VERSION: string =
  typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "0.0.0";

function readSeen(): string | null {
  try {
    return localStorage.getItem(SEEN_VERSION_KEY);
  } catch {
    // Приватный режим или заблокированное хранилище: тогда окно «Что нового»
    // просто не появится — падать из-за этого нельзя.
    return null;
  }
}

function writeSeen(version: string): void {
  try {
    localStorage.setItem(SEEN_VERSION_KEY, version);
  } catch {
    // см. readSeen()
  }
}

interface WhatsNewState {
  /** Записи к показу, самая свежая первой. Пусто — показывать нечего. */
  entries: ChangelogEntry[];
  open: boolean;
}

const _store = writable<WhatsNewState>({ entries: [], open: false });

export const whatsNew = {
  subscribe: _store.subscribe,

  /**
   * Проверить, что человек пропустил. Первый вход ничего не показывает:
   * окно рассказывает про изменения, а не про весь сервис — но версию
   * запоминает, чтобы следующее обновление он увидел.
   *
   * Решение принимается по одной строке версии; текст журнала подгружается
   * отдельно, чтобы он не тянулся в первый экран.
   */
  check(): Promise<void> {
    const seen = readSeen();
    if (seen !== APP_VERSION) writeSeen(APP_VERSION);
    if (!seen || compareVersions(seen, APP_VERSION) >= 0) {
      _store.set({ entries: [], open: false });
      return Promise.resolve();
    }
    return import("#lib/changelog.js")
      .then(({ entriesSince }) => {
        const entries = entriesSince(seen, APP_VERSION);
        _store.set({ entries, open: entries.length > 0 });
      })
      .catch(() => {
        // Журнал не подгрузился: окно просто не появится, версию мы уже записали.
      });
  },

  dismiss(): void {
    _store.update((state) => ({ ...state, open: false }));
  },
};
