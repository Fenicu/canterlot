import { intents } from "#lib/ops/intent.js";

/** Сколько ждать итога прерывания от машины, прежде чем вернуть кнопку владельцу. */
export const INTERRUPT_DEADLINE_MS = 20_000;

/** Надпись кнопки, пока прерывание не просили. */
export const INTERRUPT_IDLE = "Прервать";

/** Запрос не ушёл: кнопка предлагает повтор, лента называет исход. */
export const INTERRUPT_FAILED = "Не прервано, повторить";
export const INTERRUPT_FAILED_OUTCOME = "Прерывание не дошло";

export interface InterruptDeps {
  conversationId: string;
  /** Возвращает ответ хаба `StopOutcome`. */
  api?(conversationId: string, key: string): Promise<unknown>;
}

/**
 * Прерывание текущего хода одной сессии. Кнопка живой строки хода и пункт меню
 * сессии читают одно состояние: повторное нажатие из другого места не шлёт второй запрос.
 */
export class InterruptControl {
  label = $state(INTERRUPT_IDLE);
  running = $state(false);
  #timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly deps: InterruptDeps) {}

  async interrupt(): Promise<void> {
    if (this.running) return;
    this.running = true;
    this.label = "Прерываю…";

    let outcome: unknown;
    const conversationId = this.deps.conversationId;
    try {
      // Повтор после обрыва идёт тем же ключом: хаб не заведёт второе прерывание.
      outcome = await intents.run(`interrupt:${conversationId}`, async (key) => {
        if (this.deps.api) return this.deps.api(conversationId, key);
        const { interrupt } = await import("#lib/api/client.js");
        return (await interrupt(conversationId, key)).data;
      });
    } catch {
      // Кнопка остаётся на виду с причиной: молча вернуть «Прервать» значит скрыть сбой.
      this.label = INTERRUPT_FAILED;
      this.running = false;
      return;
    }
    if (!this.running) return;
    // Живого исполнения нет: прерывать на машине нечего, очередь при этом не встаёт.
    const requested = (outcome as { requested?: { interrupt_command_id?: unknown } } | null)?.requested;
    if (requested && requested.interrupt_command_id == null) {
      this.applyStatus("not_applicable");
      return;
    }
    // Итог приходит событием от машины; машина могла пропасть, и кнопка не ждёт вечно.
    this.#timer = setTimeout(() => {
      this.#timer = null;
      if (!this.running) return;
      this.label = "Прерывание не подтверждено";
      this.running = false;
    }, INTERRUPT_DEADLINE_MS);
  }

  applyStatus(status: string): void {
    const label = terminalInterruptLabel(status);
    if (!label) return;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.label = label;
    this.running = false;
  }

  /** Новый ход начинается с чистой кнопки, если прошлое прерывание уже закончилось. */
  reset(): void {
    if (!this.running && this.label !== INTERRUPT_IDLE) this.label = INTERRUPT_IDLE;
  }
}

const TERMINAL_INTERRUPT: Record<string, string> = {
  completed: "Прервано",
  not_applicable: "Прерывание не требуется",
  superseded: "Прерывание заменено",
  rejected_stale: "Прерывание устарело",
};

export function terminalInterruptLabel(status: string): string | null {
  return TERMINAL_INTERRUPT[status] ?? null;
}
