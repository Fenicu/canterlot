import { refreshLimits } from '#lib/api/client.js';
import { ApiError } from '#lib/api/http.js';

const WAIT_FOR_READING_MS = 10_000;

/**
 * Запрос свежего показания лимитов. Новое показание приходит событием хаба: экран передаёт
 * время самого свежего показания в `observe`, и оно закрывает ожидание.
 */
export class LimitsRefresh {
  pending = $state(false);
  note = $state('');
  #baseline = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;

  observe(updatedAt: number): void {
    if (this.pending && updatedAt > this.#baseline) this.#finish('');
  }

  async run(updatedAt: number): Promise<void> {
    if (this.pending) return;
    this.pending = true;
    this.note = '';
    this.#baseline = updatedAt;
    try {
      await refreshLimits();
    } catch (error) {
      this.#finish(failure(error));
      return;
    }
    this.#timer = setTimeout(() => this.#finish('Новых данных пока нет'), WAIT_FOR_READING_MS);
  }

  dispose(): void {
    clearTimeout(this.#timer);
  }

  #finish(message: string): void {
    clearTimeout(this.#timer);
    this.pending = false;
    this.note = message;
  }
}

function failure(error: unknown): string {
  if (error instanceof ApiError && error.code === 'too_many_requests') return 'Недавно уже обновляли, повторите через несколько секунд';
  if (error instanceof ApiError && error.code === 'machine_unavailable') return 'Воркер недоступен';
  return 'Не удалось обновить лимиты';
}
