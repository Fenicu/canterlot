import { SvelteSet } from "svelte/reactivity";
import { ApiError, NetworkError } from "#lib/api/http.js";

/**
 * Одно намерение владельца: один ключ идемпотентности до исхода и не больше одного запроса в полёте.
 * Повторное нажатие во время запроса получает тот же исход, повтор после обрыва связи идёт тем же ключом.
 */
export class Intents {
  readonly #keys = new Map<string, string>();
  readonly #flights = new Map<string, Promise<unknown>>();
  readonly #busy = new SvelteSet<string>();

  busy(scope: string): boolean {
    return this.#busy.has(scope);
  }

  run<T>(scope: string, call: (key: string) => Promise<T>): Promise<T> {
    const flying = this.#flights.get(scope);
    if (flying) return flying as Promise<T>;
    let key = this.#keys.get(scope);
    if (!key) {
      key = crypto.randomUUID();
      this.#keys.set(scope, key);
    }
    const used = key;
    // Вызов уходит в микрозадачу: запись о полёте появляется раньше любого исхода.
    const flight = Promise.resolve()
      .then(() => call(used))
      .then(
        (value) => {
          this.#keys.delete(scope);
          return value;
        },
        (err: unknown) => {
          if (!keepsKey(err)) this.#keys.delete(scope);
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
