import { SvelteSet } from "svelte/reactivity";
import { ApiError, NetworkError } from "#lib/api/http.js";

/**
 * Сколько держать ключ после обрыва связи: повтор в этом окне — тот же запрос, позже — новое нажатие.
 * Хаб помнит ключи 7 дней, и старый ключ у нового нажатия вернул бы прежний ответ, ничего не сделав.
 */
export const RETRY_WINDOW_MS = 2 * 60_000;

/**
 * Одно намерение владельца: один ключ идемпотентности до исхода и не больше одного запроса в полёте.
 * Повторное нажатие во время запроса получает тот же исход, повтор после обрыва связи в окне
 * `RETRY_WINDOW_MS` идёт тем же ключом.
 */
export class Intents {
  readonly #keys = new Map<string, { key: string; at: number }>();
  readonly #now: () => number;

  constructor(options: { now?: () => number } = {}) {
    this.#now = options.now ?? (() => Date.now());
  }
  readonly #flights = new Map<string, Promise<unknown>>();
  readonly #busy = new SvelteSet<string>();

  busy(scope: string): boolean {
    return this.#busy.has(scope);
  }

  run<T>(scope: string, call: (key: string) => Promise<T>): Promise<T> {
    const flying = this.#flights.get(scope);
    if (flying) return flying as Promise<T>;
    const kept = this.#keys.get(scope);
    const used =
      kept && this.#now() - kept.at < RETRY_WINDOW_MS ? kept.key : crypto.randomUUID();
    this.#keys.set(scope, { key: used, at: this.#now() });
    // Вызов уходит в микрозадачу: запись о полёте появляется раньше любого исхода.
    const flight = Promise.resolve()
      .then(() => call(used))
      .then(
        (value) => {
          this.#keys.delete(scope);
          return value;
        },
        (err: unknown) => {
          // Окно повтора отсчитывается от неудачи: долгий запрос не съедает его.
          if (!keepsKey(err)) this.#keys.delete(scope);
          else if (this.#keys.get(scope)?.key === used)
            this.#keys.set(scope, { key: used, at: this.#now() });
          throw err;
        },
      )
      .finally(() => {
        this.#flights.delete(scope);
        this.#busy.delete(scope);
      });
    this.#flights.set(scope, flight);
    this.#busy.add(scope);
    return flight;
  }

  /**
   * Исход виден в потоке событий (исполнение сменило состояние, сжатие началось): повторять
   * потерянный ответ больше незачем, следующее нажатие — новое намерение с новым ключом.
   */
  forget(prefix: string): void {
    for (const scope of [...this.#keys.keys()])
      if (scope.startsWith(prefix)) this.#keys.delete(scope);
  }
}

/** Исход неизвестен: хаб мог принять запрос, повтор должен прийти с тем же ключом. */
function keepsKey(err: unknown): boolean {
  if (err instanceof NetworkError) return true;
  if (err instanceof ApiError) return err.retry === "safe_same_key";
  return (
    err !== null &&
    typeof err === "object" &&
    (err as { name?: string }).name === "AbortError"
  );
}

export const intents = new Intents();
