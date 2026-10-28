import { describe, test, expect } from 'vitest';
import { stripAnsi } from '../../../src/lib/render/ansi';

describe('ansi', () => {
  test('ansi_scanner_handles_c1_terminators_and_unterminated_sequences', () => {
    expect(stripAnsi('\u009b31mred\u009b0m')).toBe('red');
    expect(stripAnsi('\u009d0;title\u009cvisible')).toBe('visible');
    expect(stripAnsi('\u001b]title\u009cvisible')).toBe('visible');
    expect(stripAnsi('\u009dtitle\u0007visible')).toBe('visible');
    expect(stripAnsi('before\u001b]unfinished')).toBe('before');
    expect(stripAnsi('before\u009dunfinished')).toBe('before');
    expect(stripAnsi('before\u001b[31')).toBe('before');
    expect(stripAnsi('before\u001b')).toBe('before');
    expect(stripAnsi('\u001b(0line\u001b(B')).toBe('line');
    expect(stripAnsi('\u009b]'.repeat(32000))).toBe('');
    expect(stripAnsi('\u001b]'.repeat(32000))).toBe('');
    expect(stripAnsi('\u001b['.repeat(32000))).toBe('');
  });

  test('ansi_quadratic_payloads_scale_roughly_linearly', () => {
    for (const opener of ['\u009b]', '\u001b]']) {
      const small = opener.repeat(4000);
      const large = opener.repeat(16000);
      for (let i = 0; i < 3; i++) stripAnsi(small);
      stripAnsi(large);
      const measure = (input: string, repetitions: number) => {
        const started = performance.now();
        for (let i = 0; i < repetitions; i++) stripAnsi(input);
        return performance.now() - started;
      };
      // Calibrate the batch, so clock resolution and JIT startup do not dominate.
      let repetitions = 1;
      while (measure(small, repetitions) < 20) repetitions *= 2;
      const smallTimes: number[] = [];
      const largeTimes: number[] = [];
      for (let i = 0; i < 5; i++) {
        smallTimes.push(measure(small, repetitions));
        largeTimes.push(measure(large, repetitions));
      }
      const median = (times: number[]) => times.sort((a, b) => a - b)[2]!;
      // Input grows 4x; quadratic backtracking grows about 16x.
      expect(median(largeTimes) / median(smallTimes)).toBeLessThan(10);
    }
  });

  test('ansi_sequences_are_stripped_from_tool_output', () => {
    // Basic CSI color sequences
    expect(stripAnsi('\u001b[31mRed text\u001b[0m')).toBe('Red text');
    expect(stripAnsi('\u001b[1;32;40mBold Green on Black\u001b[0m')).toBe('Bold Green on Black');

    // Cursor movement and line clearing
    expect(stripAnsi('\u001b[2K\rStarting task...')).toBe('\rStarting task...');
    expect(stripAnsi('\u001b[1A\u001b[2KDone!')).toBe('Done!');

    // 256 colors and truecolor
    expect(stripAnsi('\u001b[38;5;196m256 color\u001b[0m')).toBe('256 color');
    expect(stripAnsi('\u001b[38;2;255;100;50mTruecolor\u001b[0m')).toBe('Truecolor');

    // OSC hyperlinks and title
    expect(stripAnsi('\u001b]0;Terminal Title\u0007Hello')).toBe('Hello');
    expect(stripAnsi('\u001b]8;;https://example.com\u001b\\Link text\u001b]8;;\u001b\\')).toBe('Link text');

    // Plain text untouched
    expect(stripAnsi('Plain text without ANSI')).toBe('Plain text without ANSI');
    expect(stripAnsi('')).toBe('');
  });
});
