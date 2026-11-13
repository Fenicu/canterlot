/**
 * AudioWorklet диктовки. Модуль собирается Vite в отдельный файл с хешем и грузится
 * с того же адреса: blob: и встроенные скрипты под CSP производства не нужны.
 *
 * В AudioWorklet нет привычных имён TS, поэтому окружение объявлено здесь же: файл
 * исполняется только в worklet, юнит-тесты проверяют чистую математику в resample.ts.
 */
import { DictationFramer, StreamResampler, downmix } from "./resample.js";

declare const sampleRate: number;
declare const AudioWorkletProcessor: new () => { readonly port: MessagePort };
declare const registerProcessor: (name: string, processor: new () => unknown) => void;

class DictationProcessor extends AudioWorkletProcessor {
  readonly #resampler = new StreamResampler(sampleRate);
  readonly #framer = new DictationFramer();
  #running = true;
  #stopped = false;

  constructor() {
    super();
    this.port.onmessage = (event: MessageEvent) => {
      if ((event.data as { type?: string } | null)?.type !== "stop" || this.#stopped) return;
      this.#stopped = true;
      this.#flush();
      this.port.postMessage({ type: "done" });
      this.#running = false;
    };
  }

  process(inputs: Float32Array[][]): boolean {
    const channels = inputs[0];
    if (channels && channels.length > 0 && !this.#stopped) {
      const mono = downmix(channels);
      for (const frame of this.#framer.push(this.#resampler.push(mono))) this.#post(frame);
    }
    return this.#running;
  }

  #flush(): void {
    const tail = this.#framer.flush();
    if (tail) this.#post(tail);
  }

  #post(frame: Uint8Array): void {
    this.port.postMessage(frame, [frame.buffer]);
  }
}

registerProcessor("dictation", DictationProcessor);
