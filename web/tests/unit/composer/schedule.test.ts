import { describe, it, expect } from 'vitest';
import { presets, validateScheduleTime } from '$lib/composer/schedule';

describe('composer/schedule', () => {
	it('schedule_validation_follows_the_hub_window', () => {
		const now = 1_700_000_000_000;

		// 1.3a rule: now + 1 s <= run_at <= now + 366 d
		// Below 1 second -> too_soon
		expect(validateScheduleTime(now, now)).toBe('too_soon');
		expect(validateScheduleTime(now + 500, now)).toBe('too_soon');
		expect(validateScheduleTime(now + 999, now)).toBe('too_soon');
		expect(validateScheduleTime(now - 10_000, now)).toBe('too_soon');

		// Exactly now + 1 s -> ok
		expect(validateScheduleTime(now + 1_000, now)).toBe('ok');

		// In the middle of the window -> ok
		expect(validateScheduleTime(now + 3600_000, now)).toBe('ok');
		expect(validateScheduleTime(now + 30 * 86_400_000, now)).toBe('ok');

		// Exactly now + 366 days -> ok
		const maxAllowed = now + 366 * 86_400_000;
		expect(validateScheduleTime(maxAllowed, now)).toBe('ok');

		// Beyond now + 366 days -> too_far
		expect(validateScheduleTime(maxAllowed + 1, now)).toBe('too_far');
		expect(validateScheduleTime(maxAllowed + 1_000, now)).toBe('too_far');
	});

	it('presets_are_computed_from_server_corrected_now', () => {
		// Test with a known base timestamp: e.g. 2026-10-29 10:00:00 UTC
		const testDate = new Date('2026-10-29T10:00:00Z');
		const now = testDate.getTime();

		const list = presets(now);
		expect(list).toHaveLength(4);

		const labels = list.map((p) => p.label);
		expect(labels).toEqual([
			'Через 15 минут',
			'Через час',
			'Сегодня вечером',
			'Завтра утром'
		]);

		// 15 minutes
		expect(list[0]!.at).toBe(now + 15 * 60 * 1000);

		// 1 hour
		expect(list[1]!.at).toBe(now + 60 * 60 * 1000);

		// "Сегодня вечером" should be at 18:00
		const evening = new Date(list[2]!.at);
		expect(evening.getHours()).toBe(18);
		expect(evening.getMinutes()).toBe(0);
		expect(list[2]!.at).toBeGreaterThan(now);

		// "Завтра утром" should be tomorrow at 9:00
		const morning = new Date(list[3]!.at);
		expect(morning.getHours()).toBe(9);
		expect(morning.getMinutes()).toBe(0);
		expect(list[3]!.at).toBeGreaterThan(now);

		// Check evening when now is late night, e.g. 22:00
		const lateDate = new Date('2026-10-29T22:00:00Z');
		const lateNow = lateDate.getTime();
		const lateList = presets(lateNow);
		const lateEvening = new Date(lateList[2]!.at);
		expect(lateEvening.getHours()).toBe(18);
		expect(lateList[2]!.at).toBeGreaterThan(lateNow);
	});
});
