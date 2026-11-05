import { afterEach, describe, expect, it } from 'vitest';
import { readAlarmsOpen, writeAlarmsOpen } from '#lib/timeline/alarmsOpen.js';

const ONE = '00000000-0000-4000-8000-0000000000c1';
const TWO = '00000000-0000-4000-8000-0000000000c2';

describe('timeline/alarmsOpen', () => {
  afterEach(() => localStorage.clear());

  it('remembers the choice per conversation', () => {
    expect(readAlarmsOpen(ONE)).toBeNull();
    writeAlarmsOpen(ONE, true);
    expect(readAlarmsOpen(ONE)).toBe(true);
    expect(readAlarmsOpen(TWO)).toBeNull();
    writeAlarmsOpen(TWO, false);
    expect(readAlarmsOpen(ONE)).toBe(true);
    expect(readAlarmsOpen(TWO)).toBe(false);
  });

  it('broken storage reads as no choice', () => {
    localStorage.setItem('canterlot.alarms-open', 'not json');
    expect(readAlarmsOpen(ONE)).toBeNull();
  });
});
