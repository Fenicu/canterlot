import { mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

/**
 * Синтетический тон для Chromium fake media: 16 кГц, моно, 16 бит.
 * Запись голоса не нужна: проверяется, что worklet отдаёт кадры и дочищает хвост.
 */
export function fakeAudioFile() {
  const path = join(tmpdir(), 'canterlot-dictation-tone.wav');
  const rate = 16_000;
  const samples = rate;
  const data = Buffer.alloc(44 + samples * 2);
  data.write('RIFF', 0);
  data.writeUInt32LE(36 + samples * 2, 4);
  data.write('WAVE', 8);
  data.write('fmt ', 12);
  data.writeUInt32LE(16, 16);
  data.writeUInt16LE(1, 20);
  data.writeUInt16LE(1, 22);
  data.writeUInt32LE(rate, 24);
  data.writeUInt32LE(rate * 2, 28);
  data.writeUInt16LE(2, 32);
  data.writeUInt16LE(16, 34);
  data.write('data', 36);
  data.writeUInt32LE(samples * 2, 40);
  for (let n = 0; n < samples; n += 1) {
    data.writeInt16LE(Math.round(0.4 * 32_767 * Math.sin((2 * Math.PI * 440 * n) / rate)), 44 + n * 2);
  }
  mkdirSync(tmpdir(), { recursive: true });
  writeFileSync(path, data);
  return path;
}
