import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';

const at = (day: number, hour: number, minute: number) =>
	new Date(2026, 10, day, hour, minute).getTime();

describe('fmt.messageTime', () => {
	it('shows only the clock for today', () => {
		expect(fmt.messageTime(at(5, 14, 3), at(5, 20, 0))).toBe('14:03');
	});

	it('names yesterday before the clock', () => {
		expect(fmt.messageTime(at(4, 9, 7), at(5, 20, 0))).toBe('вчера 09:07');
	});

	it('shows a short date before the clock for older days', () => {
		expect(fmt.messageTime(at(5, 14, 3), at(9, 8, 0))).toBe('5 нояб. 14:03');
	});

	it('keeps the year out of the short date', () => {
		expect(fmt.messageTime(new Date(2025, 11, 31, 23, 59).getTime(), at(1, 0, 5))).toBe('31 дек. 23:59');
	});
});
