/**
 * Захват микрофона: getUserMedia → AudioWorklet, который отдаёт готовые кадры
 * по 100 мс. Граф закрывается целиком на любом конце записи.
 */
import workletUrl from "./worklet.ts?worker&url";
import type { DictationCapture } from "./session.js";

export interface CaptureHooks {
  onFrame(frame: Uint8Array): void;
  /** Дорожка кончилась, замолчала или граф не ожил: запись продолжать нельзя. */
  onLost(): void;
}

const FLUSH_TIMEOUT_MS = 400;
const RESUME_TIMEOUT_MS = 2_000;

export class MicrophoneCapture implements DictationCapture {
  readonly ready: Promise<void>;
  readonly #hooks: CaptureHooks;
  #stopped = false;
  #stream: MediaStream | null = null;
  #context: AudioContext | null = null;
  #node: AudioWorkletNode | null = null;
  #source: MediaStreamAudioSourceNode | null = null;
  #sink: GainNode | null = null;
  #finishFlush: (() => void) | null = null;
  #resumeTimer: ReturnType<typeof setTimeout> | null = null;
  #wasRunning = false;

  constructor(hooks: CaptureHooks) {
    this.#hooks = hooks;
    this.ready = this.#start();
  }

  async #start(): Promise<void> {
    if (!navigator.mediaDevices?.getUserMedia) throw domError("NotFoundError");
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
    // Отмена пришла, пока браузер спрашивал разрешение: дорожки гасим сразу.
    if (this.#stopped) {
      stopTracks(stream);
      throw domError("AbortError");
    }
    this.#stream = stream;
    for (const track of stream.getAudioTracks()) {
      track.addEventListener("ended", () => this.#lost());
      track.addEventListener("mute", () => this.#lost());
    }

    const context = new AudioContext({ latencyHint: "interactive" });
    this.#context = context;
    // Контекст мог родиться уже работающим: тогда перехода в running не будет.
    this.#wasRunning = context.state === "running";
    context.addEventListener("statechange", () => this.#stateChanged());
    // Политика автозапуска может держать контекст спящим: не ждём resume,
    // граф собирается и так, а кадры придут, когда контекст оживёт.
    if (context.state === "suspended") void context.resume().catch(() => undefined);
    try {
      await context.audioWorklet.addModule(workletUrl);
      if (this.#stopped) {
        stopTracks(stream);
        await context.close().catch(() => undefined);
        return;
      }

      const node = new AudioWorkletNode(context, "dictation", {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        channelCount: 1,
        channelCountMode: "explicit",
      });
      this.#node = node;
      node.port.onmessage = (event: MessageEvent) => {
        const data = event.data as unknown;
        if (data instanceof Uint8Array) this.#hooks.onFrame(data);
        else if ((data as { type?: string } | null)?.type === "done") this.#finishFlush?.();
      };

      const source = context.createMediaStreamSource(stream);
      const sink = context.createGain();
      sink.gain.value = 0;
      source.connect(node);
      node.connect(sink);
      sink.connect(context.destination);
      this.#source = source;
      this.#sink = sink;
    } catch (error) {
      // Без рабочего графа микрофон не должен остаться включённым.
      await this.stop();
      throw error;
    }
  }

  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#clearResumeTimer();
    stopTracks(this.#stream);
    const node = this.#node;
    this.#node = null;
    if (node) {
      let done: () => void = () => undefined;
      const flushed = new Promise<void>((resolve) => {
        done = resolve;
      });
      this.#finishFlush = done;
      const timeout = setTimeout(done, FLUSH_TIMEOUT_MS);
      try {
        node.port.postMessage({ type: "stop" });
      } catch {
        done();
      }
      await flushed;
      clearTimeout(timeout);
      this.#finishFlush = null;
      node.port.onmessage = null;
      node.disconnect();
    }
    this.#source?.disconnect();
    this.#sink?.disconnect();
    this.#source = null;
    this.#sink = null;
    const context = this.#context;
    this.#context = null;
    this.#stream = null;
    if (context && context.state !== "closed") await context.close().catch(() => undefined);
  }

  #lost(): void {
    if (this.#stopped) return;
    this.#hooks.onLost();
  }

  #stateChanged(): void {
    const context = this.#context;
    if (!context || this.#stopped) return;
    const state = context.state as string;
    if (state === "running") {
      this.#wasRunning = true;
      this.#clearResumeTimer();
      return;
    }
    // Правило «не ожил за 2 с» действует только для контекста, который уже работал:
    // стартовое suspended — это политика автозапуска, а не потеря записи.
    if (!this.#wasRunning) return;
    if (state !== "interrupted" && state !== "suspended") return;
    this.#clearResumeTimer();
    this.#resumeTimer = setTimeout(() => {
      this.#resumeTimer = null;
      if (this.#stopped) return;
      if (this.#context && (this.#context.state as string) !== "running") this.#lost();
    }, RESUME_TIMEOUT_MS);
    void context.resume().catch(() => undefined);
  }

  #clearResumeTimer(): void {
    if (this.#resumeTimer !== null) clearTimeout(this.#resumeTimer);
    this.#resumeTimer = null;
  }
}

function stopTracks(stream: MediaStream | null): void {
  for (const track of stream?.getTracks() ?? []) track.stop();
}

function domError(name: string): Error {
  return Object.assign(new Error(name), { name });
}
