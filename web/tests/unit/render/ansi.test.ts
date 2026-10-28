import { describe, test, expect } from 'vitest';
import { stripAnsi } from '../../../src/lib/render/ansi';

describe('ansi', () => {
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
