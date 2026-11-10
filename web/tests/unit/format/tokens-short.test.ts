import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';

describe('fmt.tokensShort', () => {
	it('keeps the old short forms for feed lines', () => {
		expect(fmt.tokensShort(0)).toBe('0');
		expect(fmt.tokensShort(640)).toBe('640');
		expect(fmt.tokensShort(980_000)).toBe('980 тыс.');
		expect(fmt.tokensShort(1_000_000)).toBe('1 млн');
		expect(fmt.tokensShort(1_200_000)).toBe('1,2 млн');
		expect(fmt.tokensShort(12_345_678)).toBe('12,3 млн');
	});

	it('counts billions', () => {
		expect(fmt.tokensShort(1_000_000_000)).toBe('1 млрд');
		expect(fmt.tokensShort(27_952_400_000)).toBe('28 млрд');
		expect(fmt.tokensShort(1_319_435_536)).toBe('1,3 млрд');
	});

	it('rolls a rounded thousand over into a million', () => {
		expect(fmt.tokensShort(999_949)).toBe('999,9 тыс.');
		expect(fmt.tokensShort(999_950)).toBe('1 млн');
		expect(fmt.tokensShort(999_999)).toBe('1 млн');
	});

	it('rolls a rounded million over into a billion', () => {
		expect(fmt.tokensShort(999_900_000)).toBe('999,9 млн');
		expect(fmt.tokensShort(999_950_000)).toBe('1 млрд');
		expect(fmt.tokensShort(999_999_999)).toBe('1 млрд');
	});
});
