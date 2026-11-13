import type { WebSocketRoute } from "@playwright/test";
import type { DictationStatusView } from "../../src/lib/dictation/availability";
import { validateTerms, type DictationTerm } from "../../src/lib/dictation/terms";

/** Что хаб получил за одно соединение диктовки: старт, кадры, финал и отмену. */
export interface FakeDictationConnection {
  start: Record<string, unknown> | null;
  frames: Uint8Array[];
  finish: boolean;
  cancel: boolean;
  closed: boolean;
}

/** Сценарий живой диктовки: когда прислать текст, когда оборвать связь и чем ответить на finish. */
export interface FakeDictationScript {
  /** Текст приходит через столько-то мс после «listening». */
  texts?: { afterMs: number; committed: string; tentative: string }[];
  /** `final` с reason too_long приходит сам через столько-то мс. */
  tooLongAfterMs?: number;
  /** Обрыв без final через столько-то мс. */
  closeAfterMs?: number;
}

/** Поддельная диктовка хаба: статус, словарь замен и сокет с настраиваемым поведением. */
export class FakeDictation {
  status: DictationStatusView = {
    available: false,
    reason: "disabled",
    mode: null,
    engine: null,
  };
  /** Старый хаб: маршрута статуса нет. */
  statusMissing = false;
  statusFailure = 0;
  terms: DictationTerm[] = [];
  /** Код ошибки проверки словаря вместо сохранения. */
  termsFailure: string | null = null;
  state: "listening" | "loading" = "listening";
  mode: "gpu" | "cpu" = "gpu";
  script: FakeDictationScript = {};
  finalText: string | null = null;
  finalReason: "finish" | "too_long" = "finish";
  finish: "final" | "silent" | "error" | "close" = "final";
  errorCode = "engine_failed";
  closeOnStart = false;
  /** Задержка между loading и listening. */
  loadingMs = 20;
  /** Держать `loading`, пока тест не позовёт `releaseLoading()`. */
  holdLoading = false;
  private loadingWaiters: (() => void)[] = [];
  private loadingReleased = false;
  /** Сообщения, которые хаб успел получить. */
  connections: FakeDictationConnection[] = [];

  /** Отпустить ожидающие соединения из `loading` в `listening`. */
  releaseLoading(): void {
    this.loadingReleased = true;
    for (const waiter of this.loadingWaiters.splice(0)) waiter();
  }

  connect(socket: WebSocketRoute): void {
    const record: FakeDictationConnection = {
      start: null,
      frames: [],
      finish: false,
      cancel: false,
      closed: false,
    };
    this.connections.push(record);
    const timers: ReturnType<typeof setTimeout>[] = [];
    let finished = false;
    let started = false;

    const close = (code: number) => {
      if (record.closed) return;
      record.closed = true;
      socket.close({ code });
    };

    const send = (payload: unknown) => {
      if (!record.closed) socket.send(JSON.stringify(payload));
    };

    const schedule = (ms: number, action: () => void) => {
      const timer = setTimeout(action, ms);
      timers.push(timer);
    };

    const listen = () => {
      if (record.closed) return;
      send({ type: "state", state: "listening", mode: this.mode });
      for (const update of this.script.texts ?? []) {
        schedule(update.afterMs, () =>
          send({ type: "text", committed: update.committed, tentative: update.tentative, seq: 1 }),
        );
      }
      if (this.script.tooLongAfterMs !== undefined) {
        schedule(this.script.tooLongAfterMs, () => {
          if (finished) return;
          finished = true;
          send({ type: "final", text: this.finalText ?? "", reason: "too_long" });
          close(1000);
        });
      }
      if (this.script.closeAfterMs !== undefined) {
        schedule(this.script.closeAfterMs, () => close(1011));
      }
    };

    socket.onMessage((message) => {
      if (record.closed) return;
      if (typeof message !== "string") {
        const bytes = new Uint8Array(message);
        record.frames.push(bytes);
        return;
      }
      let payload: Record<string, unknown>;
      try {
        payload = JSON.parse(message) as Record<string, unknown>;
      } catch {
        return;
      }
      if (payload.type === "start" && !started) {
        started = true;
        record.start = payload;
        if (this.closeOnStart) {
          close(1011);
          return;
        }
        if (this.state === "loading") {
          send({ type: "state", state: "loading", mode: null });
          if (this.holdLoading && !this.loadingReleased) {
            this.loadingWaiters.push(listen);
          } else {
            schedule(this.loadingMs, listen);
          }
        } else {
          listen();
        }
        return;
      }
      if (payload.type === "finish" && started) {
        record.finish = true;
        if (finished) return;
        finished = true;
        if (this.finish === "silent") return;
        if (this.finish === "error") {
          send({ type: "error", code: this.errorCode });
          close(1000);
          return;
        }
        if (this.finish === "close") {
          close(1011);
          return;
        }
        const text = this.finalText ?? "";
        send({ type: "final", text, reason: this.finalReason });
        close(1000);
        return;
      }
      if (payload.type === "cancel") record.cancel = true;
    });

    socket.onClose(() => {
      record.closed = true;
      this.loadingWaiters = this.loadingWaiters.filter((waiter) => waiter !== listen);
      for (const timer of timers) clearTimeout(timer);
    });
  }

  /** Ответ на GET /api/v1/dictation/status или null, если маршрут не про статус. */
  statusResponse(): { status: number; body: unknown } | null {
    if (this.statusMissing) {
      return { status: 404, body: { error: { code: "not_found", message: "Нет маршрута", scope: "hub_db", retry: "never", correlation_id: "00000000-0000-4000-8000-0000000000d3" } } };
    }
    if (this.statusFailure) {
      return { status: this.statusFailure, body: { error: { code: "internal", message: "Сбой", scope: "hub_db", retry: "never", correlation_id: "00000000-0000-4000-8000-0000000000d3" } } };
    }
    return { status: 200, body: this.status };
  }

  termsResponse(method: string, body: unknown): { status: number; body: unknown } {
    if (method === "GET") return { status: 200, body: { terms: this.terms } };
    const sent = (body as { terms?: DictationTerm[] } | null)?.terms ?? [];
    const code = this.termsFailure ?? validateTerms(sent);
    if (code) {
      return {
        status: 400,
        body: {
          error: {
            code,
            message: code,
            scope: "hub_db",
            retry: "never",
            correlation_id: "00000000-0000-4000-8000-0000000000d4",
          },
        },
      };
    }
    this.terms = sent.map((term) => ({ pattern: term.pattern, replacement: term.replacement }));
    return { status: 200, body: { terms: this.terms } };
  }
}

export function dictationHeaders(): Record<string, string> {
  return { "Cache-Control": "no-store", "X-Server-Time-Ms": String(Date.now()) };
}
