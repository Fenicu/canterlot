/**
 * Доступность диктовки: статус спрашивается при старте, при возврате в приложение
 * (не чаще раза в минуту) и после отказа службы; ошибки откладывают следующий запрос.
 */

export interface DictationStatusView {
  available: boolean;
  reason: null | "disabled" | "no_service" | "unhealthy";
  mode: null | "gpu" | "cpu";
  engine: null | "cold" | "warm";
}

export const STATUS_TTL_MS = 60_000;
export const BACKOFF_BASE_MS = 5_000;
export const BACKOFF_MAX_MS = 60_000;

export const UNAVAILABLE_STATUS: DictationStatusView = {
  available: false,
  reason: "no_service",
  mode: null,
  engine: null,
};

export interface AvailabilityDeps {
  fetch(): Promise<DictationStatusView>;
  now(): number;
  schedule(fn: () => void, ms: number): () => void;
}

const REASON_TEXT: Record<NonNullable<DictationStatusView["reason"]>, string> = {
  disabled: "Диктовка выключена в настройках службы распознавания",
  no_service: "Служба распознавания не подключена",
  unhealthy: "Служба распознавания не отвечает",
};

/** Состояние службы словами: причина отказа или чем она готова помочь. */
export function statusText(status: DictationStatusView | null): string {
  if (!status) return "Доступность диктовки неизвестна";
  if (!status.available) return status.reason ? REASON_TEXT[status.reason] : "Диктовка недоступна";
  if (status.mode === "gpu") return "Доступна, движок на видеокарте";
  if (status.mode === "cpu") return "Доступна, медленный режим на процессоре";
  if (status.engine === "cold") return "Доступна, движок загрузится при первой записи";
  return "Доступна";
}

export class DictationAvailability {
  readonly #deps: AvailabilityDeps;
  #status: DictationStatusView | null = null;
  #failures = 0;
  #attemptAt: number | null = null;
  #running: Promise<void> | null = null;

  constructor(deps: AvailabilityDeps) {
    this.#deps = deps;
  }

  get status(): DictationStatusView | null {
    return this.#status;
  }

  get failures(): number {
    return this.#failures;
  }

  get due(): boolean {
    if (this.#running) return false;
    if (this.#attemptAt === null) return true;
    const wait =
      this.#failures > 0
        ? Math.min(BACKOFF_MAX_MS, BACKOFF_BASE_MS * 2 ** (this.#failures - 1))
        : STATUS_TTL_MS;
    return this.#deps.now() - this.#attemptAt >= wait;
  }

  /** Следующий ensure пойдёт в сеть: служба отказала прямо во время диктовки. */
  invalidate(): void {
    this.#attemptAt = null;
    this.#failures = 0;
    this.#status = null;
  }

  reasonText(): string {
    return statusText(this.#status);
  }

  ensure(force = false): Promise<void> {
    if (this.#running) return this.#running;
    if (!force && !this.due) return Promise.resolve();
    const request = this.#load();
    this.#running = request;
    void request.finally(() => {
      if (this.#running === request) this.#running = null;
    });
    return request;
  }

  async #load(): Promise<void> {
    this.#attemptAt = this.#deps.now();
    try {
      const next = await this.#deps.fetch();
      this.#status = {
        available: next.available === true,
        reason: next.reason ?? null,
        mode: next.mode ?? null,
        engine: next.engine ?? null,
      };
      this.#failures = 0;
    } catch (error) {
      // Старый хаб без маршрута и пропавшая сеть для владельца выглядят одинаково: службы нет.
      this.#status = { ...UNAVAILABLE_STATUS };
      this.#failures += 1;
      // Отказ без входа ничего не говорит о службе: после входа спрашиваем сразу, без паузы.
      const status = (error as { status?: number } | null)?.status;
      const name = (error as { name?: string } | null)?.name;
      if (status === 401 || name === "AuthRequired") this.#attemptAt = null;
    }
  }
}
