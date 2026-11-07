import { describe, expect, test } from 'vitest';
import { resetPhrase, spanLong, spanText, untilLong, untilShort, untilText } from '#lib/format/countdown.js';

const S = 1000;
const MIN = 60 * S;
const H = 60 * MIN;
const D = 24 * H;
const N = ' ';

describe('countdown to a reset', () => {
  test.each([
    [59 * S, 'меньше минуты', '<1м', 'меньше чем через минуту'],
    [MIN, `через 1${N}мин`, '1м', 'через 1 минуту'],
    [MIN + 59 * S, `через 1${N}мин`, '1м', 'через 1 минуту'],
    [2 * MIN, `через 2${N}мин`, '2м', 'через 2 минуты'],
    [25 * MIN, `через 25${N}мин`, '25м', 'через 25 минут'],
    [59 * MIN + 59 * S, `через 59${N}мин`, '59м', 'через 59 минут'],
    [H, `через 1${N}ч`, '1ч', 'через 1 час'],
    [2 * H + 15 * MIN, `через 2${N}ч 15${N}мин`, '2ч15м', 'через 2 часа 15 минут'],
    [21 * H + 1 * MIN, `через 21${N}ч 1${N}мин`, '21ч1м', 'через 21 час 1 минуту'],
    [47 * H + 59 * MIN, `через 47${N}ч 59${N}мин`, '47ч59м', 'через 47 часов 59 минут'],
    [48 * H, `через 2${N}д`, '2д', 'через 2 дня'],
    [3 * D, `через 3${N}д`, '3д', 'через 3 дня'],
    [3 * D + 4 * H + 30 * MIN, `через 3${N}д 4${N}ч`, '3д4ч', 'через 3 дня 4 часа'],
    [5 * D + 1 * H, `через 5${N}д 1${N}ч`, '5д1ч', 'через 5 дней 1 час'],
    [21 * D, `через 21${N}д`, '21д', 'через 21 день']
  ])('%d ms before the reset', (ms, text, short, long) => {
    expect(untilText(ms)).toBe(text);
    expect(untilShort(ms)).toBe(short);
    expect(untilLong(ms)).toBe(long);
  });

  test('a reset that already passed shows nothing', () => {
    for (const ms of [0, -1, -H, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(untilText(ms)).toBe('');
      expect(untilShort(ms)).toBe('');
      expect(untilLong(ms)).toBe('');
      expect(spanText(ms)).toBe('');
    }
  });

  test('span drops «через» for the narrow strip', () => {
    expect(spanText(2 * H + 15 * MIN)).toBe(`2${N}ч 15${N}мин`);
    expect(spanText(30 * S)).toBe('меньше минуты');
  });

  test('coarse forecast rounds to whole hours and days', () => {
    expect(untilText(3 * H + 20 * MIN, true)).toBe(`через 3${N}ч`);
    expect(untilText(3 * H + 40 * MIN, true)).toBe(`через 4${N}ч`);
    expect(untilText(40 * MIN, true)).toBe(`через 40${N}мин`);
    expect(untilText(47 * H + 40 * MIN, true)).toBe(`через 2${N}д`);
    expect(untilText(9 * D + 12 * H, true)).toBe(`через 9${N}д 12${N}ч`);
    expect(spanLong(11 * H, true)).toBe('11 часов');
    expect(untilLong(22 * H, true)).toBe('через 22 часа');
  });

  test('reset phrase reads naturally under a minute', () => {
    expect(resetPhrase(untilText(2 * H))).toBe(`сброс через 2${N}ч`);
    expect(resetPhrase(untilText(10 * S))).toBe('сброс меньше чем через минуту');
    expect(resetPhrase('')).toBe('');
  });
});
