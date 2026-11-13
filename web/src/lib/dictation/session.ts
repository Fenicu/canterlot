/**
 * Одна диктовка: подключение к хабу, поток кадров, живой текст и завершение без потерь.
 * Класс не зависит от DOM: микрофон, сокет и таймеры приходят снаружи, поэтому правила
 * (ровно одна вставка, поздний итог игнорируется, срок ожидания) проверяются юнит-тестами.
 */
import { frameLevel } from "#lib/dictation/resample.js";

export type DictationMode = "gpu" | "cpu";
export type DictationPhase =
  | "starting"
  | "loading"
  | "listening"
  | "finishing"
  | "review"
  | "failed"
  | "closed";

export interface DictationView {
  phase: DictationPhase;
  mode: DictationMode | null;
  committed: string;
  tentative: string;
  message: string;
  notice: string;
  slow: boolean;
  canInsert: boolean;
  timedOut: boolean;
  level: number;
  startedAt: number;
  mic: "denied" | "missing" | null;
}

export interface DictationSocket {
  send(data: string | ArrayBuffer): void;
  close(): void;
  readonly bufferedAmount: number;
}

export interface DictationConnectHandlers {
  open(): void;
  message(data: string): void;
  close(code: number): void;
}

export interface DictationCapture {
  /** Разрешение микрофона и запуск графа; reject — захват не начался. */
  ready: Promise<void>;
  /** Останавливает дорожки, дочищает последний кадр и закрывает граф. */
  stop(): Promise<void>;
}

export interface DictationDeps {
  connect(handlers: DictationConnectHandlers): DictationSocket;
  startCapture(hooks: { onFrame(frame: Uint8Array): void; onLost(): void }): DictationCapture;
  schedule(fn: () => void, ms: number): () => void;
  now(): number;
  client: string;
  onview(view: DictationView): void;
  /** Ровно один раз за диктовку: текст, который нужно вставить в черновик. */
  oninsert(text: string, notice: string): void;
  onunavailable?(): void;
}

const ERROR_TEXT: Record<string, string> = {
  speech_unavailable: "Служба распознавания недоступна",
  busy: "Диктовка уже идёт на другом устройстве",
  replaced: "Диктовку начали на другом устройстве",
  bad_request: "Хаб отклонил запрос диктовки",
  bad_audio: "Хаб не принял звук",
  idle: "Звук перестал поступать",
  start_timeout: "Служба не успела загрузить модель",
  lifetime: "Достигнут предел длительности диктовки",
  engine_failed: "Сбой распознавания",
};

export const TOO_LONG_NOTICE = "Достигнут предел 10 минут";
export const TIMEOUT_MESSAGE = "Не дождались итога распознавания";
export const CONNECTION_LOST_MESSAGE = "Связь прервалась";
export const SLOW_LINK_BYTES = 64 * 1024;
const SLOW_LINK_CHECK_MS = 500;
const SLOW_LINK_CHECKS = 2;
const GPU_DEADLINE_MS = 5_000;
const CPU_DEADLINE_MS = 30_000;
const MAX_QUEUED_FRAMES = 100;

export function dictationErrorText(code: string): string {
  return ERROR_TEXT[code] ?? "Диктовка прервалась из-за ошибки";
}

function emptyView(startedAt: number): DictationView {
  return {
    phase: "starting",
    mode: null,
    committed: "",
    tentative: "",
    message: "Подключаю микрофон…",
    notice: "",
    slow: false,
    canInsert: false,
    timedOut: false,
    level: 0,
    startedAt,
    mic: null,
  };
}

export class DictationSession {
  readonly #deps: DictationDeps;
  #view: DictationView;
  #socket: DictationSocket | null = null;
  #capture: DictationCapture | null = null;
  #open = false;
  #closedSocket = false;
  #micReady = false;
  #hubState: "loading" | "listening" | null = null;
  #lastSeq = 0;
  #acceptFrames = true;
  #settled = false;
  #finishing = false;
  #queued: Uint8Array[] = [];
  #timers = new Set<() => void>();
  #slowChecks = 0;

  constructor(deps: DictationDeps) {
    this.#deps = deps;
    this.#view = emptyView(deps.now());
  }

  get view(): DictationView {
    return { ...this.#view };
  }

  async start(): Promise<void> {
    this.#emit();
    this.#socket = this.#deps.connect({
      open: () => this.#opened(),
      message: (data) => this.#message(data),
      close: () => this.#lost(),
    });
    this.#capture = this.#deps.startCapture({
      onFrame: (frame) => this.#frame(frame),
      onLost: () => this.#loseMicrophone(),
    });
    this.#armSlowChecks();
    try {
      await this.#capture.ready;
    } catch (error) {
      this.#captureFailed(error);
      return;
    }
    if (this.#settled) return;
    this.#micReady = true;
    this.#refreshPhase();
  }

  /** «Готово»: дочищаем кадр, просим итог и ждём его не дольше срока режима. */
  finish(): void {
    if (this.#settled || this.#finishing) return;
    this.#finishing = true;
    this.#refreshPhase();
    const delay = this.#view.mode === "gpu" ? GPU_DEADLINE_MS : CPU_DEADLINE_MS;
    void this.#stopCapture().then(() => {
      if (this.#settled) return;
      if (!this.#open) {
        this.#review(CONNECTION_LOST_MESSAGE);
        return;
      }
      this.#sendText({ type: "finish" });
      this.#acceptFrames = false;
      this.#after(delay, () => this.#deadline());
    });
  }

  /** «Отмена»: кадры и распознанный текст не нужны. */
  cancel(): void {
    if (this.#settled) {
      if (this.#view.phase === "review" || this.#view.phase === "failed") {
        this.#view = { ...this.#view, phase: "closed", message: "", canInsert: false };
        this.#emit();
      }
      return;
    }
    this.#settled = true;
    this.#acceptFrames = false;
    this.#clearTimers();
    if (this.#open) this.#sendText({ type: "cancel" });
    this.#closeSocket();
    void this.#stopCapture();
    this.#view = { ...emptyView(this.#view.startedAt), phase: "closed", message: "" };
    this.#emit();
  }

  /** «Вставить что есть (неполно)»: единственный шанс вставить снимок, после — только отмена. */
  insertPartial(): void {
    if (this.#view.phase !== "review" || this.#settled) return;
    const text = this.#snapshot();
    if (!text) return;
    this.#settled = true;
    this.#acceptFrames = false;
    this.#clearTimers();
    this.#view = { ...this.#view, phase: "closed", message: "", canInsert: false };
    this.#emit();
    this.#deps.oninsert(text, "");
  }

  /** Уход с экрана: микрофон выключен, сокет закрыт, ничего не вставляется. */
  dispose(): void {
    this.cancel();
  }

  #opened(): void {
    if (this.#settled) return;
    this.#open = true;
    this.#sendText({ type: "start", sample_rate: 16_000, format: "s16le", client: this.#deps.client });
    for (const frame of this.#queued.splice(0)) this.#sendFrame(frame);
  }

  #message(data: string): void {
    if (!this.#accepting()) return;
    let payload: Record<string, unknown>;
    try {
      payload = JSON.parse(data) as Record<string, unknown>;
    } catch {
      return;
    }
    if (payload.type === "state") this.#hubStateMessage(payload);
    else if (payload.type === "text") this.#textMessage(payload);
    else if (payload.type === "final") this.#finalMessage(payload);
    else if (payload.type === "error") {
      const code = typeof payload.code === "string" ? payload.code : "";
      this.#errorMessage(code);
    }
  }

  #hubStateMessage(payload: Record<string, unknown>): void {
    if (payload.state === "listening") {
      this.#hubState = "listening";
      this.#view.mode = payload.mode === "cpu" ? "cpu" : payload.mode === "gpu" ? "gpu" : null;
    } else if (payload.state === "loading") {
      this.#hubState = "loading";
      this.#view.mode = null;
    } else {
      return;
    }
    this.#refreshPhase();
  }

  #textMessage(payload: Record<string, unknown>): void {
    if (typeof payload.committed !== "string" || typeof payload.tentative !== "string") return;
    const seq = typeof payload.seq === "number" ? payload.seq : 0;
    if (seq <= this.#lastSeq) return;
    this.#lastSeq = seq;
    this.#view = {
      ...this.#view,
      committed: payload.committed,
      tentative: payload.tentative,
    };
    this.#emit();
  }

  #finalMessage(payload: Record<string, unknown>): void {
    const text = typeof payload.text === "string" ? payload.text : this.#snapshot();
    const tooLong = payload.reason === "too_long";
    this.#acceptFrames = false;
    this.#settled = true;
    this.#clearTimers();
    void this.#stopCapture();
    this.#closeSocket();
    this.#view = { ...this.#view, phase: "closed", message: "", canInsert: false };
    this.#emit();
    this.#deps.oninsert(text, tooLong ? TOO_LONG_NOTICE : "");
  }

  #errorMessage(code: string): void {
    if (code === "speech_unavailable") this.#deps.onunavailable?.();
    this.#review(dictationErrorText(code));
  }

  /** Обрыв без итога: снимок текста остаётся у владельца, но сам он не вставляется. */
  #lost(): void {
    this.#open = false;
    this.#closedSocket = true;
    if (this.#settled) return;
    this.#review(CONNECTION_LOST_MESSAGE);
  }

  #loseMicrophone(): void {
    if (!this.#accepting() || this.#finishing) return;
    this.finish();
  }

  #deadline(): void {
    if (!this.#accepting()) return;
    this.#view.timedOut = true;
    this.#review(TIMEOUT_MESSAGE);
  }

  #captureFailed(error: unknown): void {
    // Запись уже заканчивается по воле владельца: отказ микрофона её не отменяет.
    if (this.#settled || this.#finishing) return;
    const name = (error as { name?: string } | null)?.name ?? "";
    const denied = name === "NotAllowedError" || name === "SecurityError";
    const missing = name === "NotFoundError" || name === "OverconstrainedError";
    this.#settled = true;
    this.#acceptFrames = false;
    this.#clearTimers();
    if (this.#open) this.#sendText({ type: "cancel" });
    this.#closeSocket();
    void this.#stopCapture();
    this.#view = {
      ...this.#view,
      phase: "failed",
      message: denied
        ? "Разрешите микрофон в настройках браузера или приложения"
        : missing
          ? "Микрофон не найден"
          : "Не удалось получить доступ к микрофону",
      mic: denied ? "denied" : missing ? "missing" : null,
    };
    this.#emit();
  }

  /** Отказ после записи: показываем снимок и даём выбрать между им и отменой. */
  #review(message: string): void {
    if (!this.#accepting()) return;
    this.#acceptFrames = false;
    this.#clearTimers();
    void this.#stopCapture();
    this.#closeSocket();
    const canInsert = this.#snapshot().length > 0;
    this.#view = {
      ...this.#view,
      phase: canInsert ? "review" : "failed",
      message,
      canInsert,
    };
    this.#emit();
  }

  /** Сокет ещё принимает сообщения, а решение о вставке не принято. */
  #accepting(): boolean {
    if (this.#settled) return false;
    const phase = this.#view.phase;
    return phase === "starting" || phase === "loading" || phase === "listening" || phase === "finishing";
  }

  #snapshot(): string {
    return `${this.#view.committed}${this.#view.tentative}`.trim();
  }

  #frame(frame: Uint8Array): void {
    if (!this.#accepting() || !this.#acceptFrames) return;
    this.#view.level = frameLevel(frame);
    if (this.#open) this.#sendFrame(frame);
    else if (this.#queued.length < MAX_QUEUED_FRAMES) this.#queued.push(frame);
    this.#emit();
  }

  #sendFrame(frame: Uint8Array): void {
    if (!this.#socket) return;
    const copy = new Uint8Array(frame);
    this.#socket.send(copy.buffer);
  }

  #sendText(payload: Record<string, unknown>): void {
    try {
      this.#socket?.send(JSON.stringify(payload));
    } catch {
      // Сокет уже закрылся сам: об этом сообщит его close.
    }
  }

  #refreshPhase(): void {
    if (!this.#accepting()) return;
    if (this.#finishing) {
      this.#view = { ...this.#view, phase: "finishing", slow: false };
    } else if (!this.#micReady) {
      this.#view = { ...this.#view, phase: "starting" };
    } else if (this.#hubState === "listening") {
      this.#view = { ...this.#view, phase: "listening" };
    } else {
      this.#view = { ...this.#view, phase: "loading" };
    }
    this.#emit();
  }

  #emit(): void {
    const view = this.#view;
    if (view.phase === "starting") view.message = "Подключаю микрофон…";
    else if (view.phase === "loading") view.message = "Загружаю модель…";
    else if (view.phase === "listening") {
      view.message = view.slow ? "Связь медленная" : view.mode === "cpu" ? "Медленный режим" : "Слушаю";
    } else if (view.phase === "finishing") view.message = "Распознаю…";
    this.#deps.onview({ ...view });
  }

  #armSlowChecks(): void {
    this.#after(SLOW_LINK_CHECK_MS, () => {
      if (!this.#accepting() || this.#finishing) return;
      const buffered = this.#socket?.bufferedAmount ?? 0;
      if (buffered > SLOW_LINK_BYTES) {
        this.#slowChecks += 1;
        this.#view.slow = true;
        this.#emit();
        if (this.#slowChecks >= SLOW_LINK_CHECKS) {
          this.finish();
          return;
        }
      } else if (this.#view.slow) {
        this.#slowChecks = 0;
        this.#view.slow = false;
        this.#emit();
      }
      this.#armSlowChecks();
    });
  }

  #after(ms: number, fn: () => void): void {
    let cancel = () => undefined as void;
    const timer = this.#deps.schedule(() => {
      this.#timers.delete(cancel);
      fn();
    }, ms);
    cancel = () => {
      this.#timers.delete(cancel);
      timer();
    };
    this.#timers.add(cancel);
  }

  #clearTimers(): void {
    for (const cancel of [...this.#timers]) cancel();
    this.#timers.clear();
  }

  async #stopCapture(): Promise<void> {
    const capture = this.#capture;
    this.#capture = null;
    if (!capture) return;
    try {
      await capture.stop();
    } catch {
      // Граф уже мог закрыться сам: важнее не потерять итог.
    }
  }

  #closeSocket(): void {
    if (this.#closedSocket) return;
    this.#closedSocket = true;
    this.#open = false;
    try {
      this.#socket?.close();
    } catch {
      // Повторное закрытие сокета — не ошибка.
    }
  }
}
