/**
 * Пересчёт звука микрофона к формату диктовки: 16 кГц, моно, s16le, кадры по 100 мс.
 * Модуль чистый: его импортирует и AudioWorklet, и юнит-тесты.
 */

export const TARGET_RATE = 16_000;
export const FRAME_SAMPLES = 1_600;
export const FRAME_BYTES = 3_200;

/** Микширование каналов в моно: сумма делится на число каналов. */
export function downmix(channels: Float32Array[]): Float32Array {
  const first = channels[0];
  if (!first) return new Float32Array(0);
  if (channels.length === 1) return first;
  const out = new Float32Array(first.length);
  for (const channel of channels) {
    const length = Math.min(out.length, channel.length);
    for (let index = 0; index < length; index += 1) out[index] = out[index]! + channel[index]!;
  }
  for (let index = 0; index < out.length; index += 1) out[index] = out[index]! / channels.length;
  return out;
}

/**
 * Линейный пересчёт потока целиком: дробная позиция и хвост переносятся между кусками,
 * поэтому на стыке кусков нет щелчка. Позиция следующего отсчёта — точное кратное шага,
 * так что длина на выходе не уплывает за секунды записи.
 */
export class StreamResampler {
  readonly outputRate: number;
  readonly #step: number;
  #carry = new Float32Array(0);
  #position = 0;

  constructor(inputRate: number, outputRate: number = TARGET_RATE) {
    if (!(inputRate > 0) || !(outputRate > 0)) throw new RangeError("Частота должна быть больше нуля");
    this.outputRate = outputRate;
    this.#step = inputRate / outputRate;
  }

  push(input: Float32Array): Float32Array {
    if (input.length === 0) return new Float32Array(0);
    const data = new Float32Array(this.#carry.length + input.length);
    data.set(this.#carry);
    data.set(input, this.#carry.length);
    const out: number[] = [];
    let position = this.#position;
    while (position < data.length) {
      const index = Math.floor(position);
      if (index >= data.length - 1) {
        // Для интерполяции нужен следующий отсчёт; последний берём как есть, если попали точно в него.
        if (index === data.length - 1 && position === index) {
          out.push(data[index]!);
          position += this.#step;
        }
        break;
      }
      const fraction = position - index;
      const left = data[index]!;
      out.push(left + (data[index + 1]! - left) * fraction);
      position += this.#step;
    }
    const keep = Math.min(Math.floor(position), data.length);
    this.#carry = data.slice(keep);
    this.#position = position - keep;
    return Float32Array.from(out);
  }
}

export function toInt16(sample: number): number {
  if (!Number.isFinite(sample)) return 0;
  if (sample <= -1) return -32_768;
  if (sample >= 1) return 32_767;
  return Math.round(sample * 32_768);
}

/** Копит отсчёты и отдаёт ровно по 3200 байт; последний кадр при завершении может быть короче. */
export class DictationFramer {
  #pending: number[] = [];

  get pendingSamples(): number {
    return this.#pending.length;
  }

  push(samples: Float32Array): Uint8Array[] {
    for (const sample of samples) this.#pending.push(toInt16(sample));
    const frames: Uint8Array[] = [];
    while (this.#pending.length >= FRAME_SAMPLES) frames.push(this.#take(FRAME_SAMPLES));
    return frames;
  }

  flush(): Uint8Array | null {
    return this.#pending.length > 0 ? this.#take(this.#pending.length) : null;
  }

  #take(count: number): Uint8Array {
    const samples = this.#pending.splice(0, count);
    const bytes = new Uint8Array(count * 2);
    const view = new DataView(bytes.buffer);
    for (let index = 0; index < samples.length; index += 1) view.setInt16(index * 2, samples[index]!, true);
    return bytes;
  }
}

/** Пик громкости кадра в долях шкалы: панель рисует по нему столбики уровня. */
export function frameLevel(frame: Uint8Array | ArrayBuffer): number {
  const view = frame instanceof Uint8Array ? new DataView(frame.buffer, frame.byteOffset, frame.byteLength) : new DataView(frame);
  let peak = 0;
  for (let offset = 0; offset + 1 < view.byteLength; offset += 2) {
    peak = Math.max(peak, Math.abs(view.getInt16(offset, true)) / 32_768);
  }
  return Math.min(1, peak);
}
