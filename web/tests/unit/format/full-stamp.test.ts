import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';

const at = (iso: string) => Date.parse(iso);

describe('fmt.fullStamp', () => {
	it('names the weekday and shows the full date with seconds', () => {
		expect(fmt.fullStamp(at('2026-11-06T13:35:12Z'), 'Europe/Moscow')).toBe(
			'пятница, 6 ноября 2026, 16:35:12'
		);
	});

	it('uses the requested timezone and rolls the date over midnight', () => {
		expect(fmt.fullStamp(at('2026-11-06T23:35:12Z'), 'Europe/Moscow')).toBe(
			'суббота, 7 ноября 2026, 02:35:12'
		);
		expect(fmt.fullStamp(at('2026-11-06T13:35:12Z'), 'UTC')).toBe(
			'пятница, 6 ноября 2026, 13:35:12'
		);
	});

	it('keeps the day without a leading zero', () => {
		expect(fmt.fullStamp(at('2026-11-06T09:05:02Z'), 'UTC')).toBe(
			'пятница, 6 ноября 2026, 09:05:02'
		);
	});

	it('renders nothing for a missing timestamp', () => {
		expect(fmt.fullStamp(Number.NaN)).toBe('');
	});
});
