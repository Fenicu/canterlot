import { expect, test } from 'vitest';
import { parseServerFrame, seq } from '../../../src/lib/sync/wire';

test('wire_ignores_unknown_frames_and_keeps_decimal_precision', () => {
  expect(parseServerFrame('{')).toEqual({ op: 'unknown' });
  expect(parseServerFrame('{"op":"future"}')).toEqual({ op: 'unknown' });
  expect(parseServerFrame('{"op":"pong","t":2,"future":true}')).toMatchObject({ op: 'pong', t: 2 });
  expect(seq.inc('9007199254740993')).toBe('9007199254740994');
  expect(seq.cmp('9007199254740993', '9007199254740992')).toBe(1);
});
