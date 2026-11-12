import { ApiError } from "#lib/api/http.js";

/** Владелец закрыл окно подтверждения: действие не выполняется, ошибку не показываем. */
export class StepUpCancelled extends Error {
  constructor() {
    super("Подтверждение отменено");
    this.name = "StepUpCancelled";
  }
}

/**
 * Действия управления входом требуют недавнего входа: на `reauth_required` экран спрашивает
 * пароль или ключ доступа и повторяет действие один раз.
 */
class StepUp {
  open = $state(false);
  #pending: { resolve: () => void; reject: (error: Error) => void } | null = null;

  async run<T>(action: () => Promise<T>): Promise<T> {
    try {
      return await action();
    } catch (error) {
      if (!(error instanceof ApiError && error.code === "reauth_required")) throw error;
    }
    await this.ask();
    return await action();
  }

  ask(): Promise<void> {
    this.#pending?.reject(new StepUpCancelled());
    this.open = true;
    return new Promise((resolve, reject) => {
      this.#pending = { resolve, reject };
    });
  }

  confirmed(): void {
    this.open = false;
    const pending = this.#pending;
    this.#pending = null;
    pending?.resolve();
  }

  cancel(): void {
    this.open = false;
    const pending = this.#pending;
    this.#pending = null;
    pending?.reject(new StepUpCancelled());
  }
}

export const stepUp = new StepUp();
