import { describe, expect, it } from "vitest";
import {
  DictationFramer,
  FRAME_BYTES,
  FRAME_SAMPLES,
  StreamResampler,
  downmix,
  toInt16,
} from "#lib/dictation/resample.js";

/** Синус с шагом исходной частоты: так проверяется и длина, и форма сигнала. */
function sine(rate: number, seconds: number, frequency = 440): Float32Array {
  const out = new Float32Array(Math.round(rate * seconds));
  for (let n = 0; n < out.length; n += 1) {
    out[n] = 0.6 * Math.sin((2 * Math.PI * frequency * n) / rate);
  }
  return out;
}

function chunks(samples: Float32Array, size: number): Float32Array[] {
  const out: Float32Array[] = [];
  for (let at = 0; at < samples.length; at += size) out.push(samples.subarray(at, at + size));
  return out;
}

function resampleAll(rate: number, seconds: number, chunk: number): Float32Array {
  const resampler = new StreamResampler(rate);
  const parts = chunks(sine(rate, seconds), chunk).map((part) => resampler.push(part));
  const length = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Float32Array(length);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

describe("StreamResampler", () => {
  it("keeps_signal_shape_within_tolerance_at_48_khz", () => {
    const out = resampleAll(48_000, 1, 128);
    expect(Math.abs(out.length - 16_000)).toBeLessThanOrEqual(3);
    for (let n = 1; n < out.length - 1; n += 1) {
      const expected = 0.6 * Math.sin((2 * Math.PI * 440 * n) / 16_000);
      expect(Math.abs(out[n]! - expected)).toBeLessThan(0.02);
    }
  });

  it("keeps_length_and_shape_at_44_1_khz", () => {
    const out = resampleAll(44_100, 1, 128);
    expect(Math.abs(out.length - 16_000)).toBeLessThanOrEqual(3);
    for (let n = 1; n < out.length - 1; n += 1) {
      const expected = 0.6 * Math.sin((2 * Math.PI * 440 * n) / 16_000);
      expect(Math.abs(out[n]! - expected)).toBeLessThan(0.03);
    }
  });

  it("keeps_values_continuous_across_chunk_boundaries", () => {
    const resampler = new StreamResampler(48_000);
    const parts = chunks(sine(48_000, 0.5), 128).map((part) => resampler.push(part));
    const out = Float32Array.from(parts.flatMap((part) => [...part]));
    // Скачок между соседними отсчётами больше шага синуса означал бы разрыв на стыке кусков.
    for (let n = 1; n < out.length; n += 1) {
      expect(Math.abs(out[n]! - out[n - 1]!)).toBeLessThan(0.12);
    }
  });

  it("passes_sixteen_kilohertz_input_through_unchanged", () => {
    const source = sine(16_000, 0.1);
    const out = new StreamResampler(16_000).push(source);
    expect(out.length).toBe(source.length);
    expect(out[100]).toBeCloseTo(source[100]!, 5);
  });
});

describe("downmix", () => {
  it("averages_stereo_channels_into_one", () => {
    const left = Float32Array.from([1, 0, -1]);
    const right = Float32Array.from([0, 1, -1]);
    expect([...downmix([left, right])]).toEqual([0.5, 0.5, -1]);
  });

  it("keeps_a_single_channel_as_is", () => {
    const mono = Float32Array.from([0.25, -0.25]);
    expect([...downmix([mono])]).toEqual([0.25, -0.25]);
  });
});

describe("toInt16", () => {
  it("clips_beyond_full_scale", () => {
    expect(toInt16(2)).toBe(32767);
    expect(toInt16(-2)).toBe(-32768);
  });

  it("rounds_mid_scale_samples", () => {
    expect(toInt16(0)).toBe(0);
    expect(toInt16(0.5)).toBe(16384);
    expect(toInt16(-0.5)).toBe(-16384);
  });
});

describe("DictationFramer", () => {
  it("emits_exactly_three_thousand_two_hundred_byte_frames", () => {
    const framer = new DictationFramer();
    const frames = framer.push(new Float32Array(FRAME_SAMPLES * 2));
    expect(FRAME_BYTES).toBe(3_200);
    expect(frames.map((frame) => frame.length)).toEqual([FRAME_BYTES, FRAME_BYTES]);
    expect(framer.pendingSamples).toBe(0);
  });

  it("joins_partial_chunks_into_one_frame", () => {
    const framer = new DictationFramer();
    expect(framer.push(new Float32Array(800))).toHaveLength(0);
    const frames = framer.push(new Float32Array(800));
    expect(frames).toHaveLength(1);
    expect(frames[0]!.length).toBe(FRAME_BYTES);
  });

  it("flushes_the_last_partial_frame", () => {
    const framer = new DictationFramer();
    framer.push(new Float32Array(FRAME_SAMPLES + 400));
    const tail = framer.flush();
    expect(tail).not.toBeNull();
    expect(tail!.length).toBe(800);
    expect(framer.pendingSamples).toBe(0);
    expect(framer.flush()).toBeNull();
  });

  it("writes_little_endian_samples", () => {
    const framer = new DictationFramer();
    const samples = new Float32Array(FRAME_SAMPLES);
    samples[0] = 1;
    samples[1] = -1;
    samples[2] = 0.5;
    const frame = framer.push(samples)[0]!;
    const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
    expect(view.getInt16(0, true)).toBe(32767);
    expect(view.getInt16(2, true)).toBe(-32768);
    expect(view.getInt16(4, true)).toBe(16384);
  });
});
