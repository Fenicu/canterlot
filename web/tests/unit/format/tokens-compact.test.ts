import { describe, expect, it } from 'vitest';
import { fmt } from '#lib/format/ru.js';

describe('fmt.tokensCompact', () => {
	it('keeps counts below a million grouped as before', () => {
		expect(fmt.tokensCompact(0)).toBe('0');
		expect(fmt.tokensCompact(129_000)).toBe('129 000');
		expect(fmt.tokensCompact(999_999)).toBe('999 999');
	});

	it('compacts millions with one decimal for hundreds', () => {
		expect(fmt.tokensCompact(4_120_998)).toBe('4,12 млн');
		expect(fmt.tokensCompact(516_733_981)).toBe('516,7 млн');
		expect(fmt.tokensCompact(1_000_000)).toBe('1 млн');
	});

	it('compacts billions with two decimals below ten', () => {
		expect(fmt.tokensCompact(1_319_435_536)).toBe('1,32 млрд');
		expect(fmt.tokensCompact(123_456_789_012)).toBe('123,5 млрд');
		expect(fmt.tokensCompact(1_000_000_000)).toBe('1 млрд');
	});

	it('rounds up across the million boundary', () => {
		expect(fmt.tokensCompact(999_999_999)).toBe('1 млрд');
	});

	it('can keep thousands short for narrow columns', () => {
		expect(fmt.tokensCompact(0, { short: true })).toBe('0');
		expect(fmt.tokensCompact(2_600, { short: true })).toBe('2,6 тыс.');
		expect(fmt.tokensCompact(237_900, { short: true })).toBe('237,9 тыс.');
		expect(fmt.tokensCompact(999_950, { short: true })).toBe('1 млн');
		expect(fmt.tokensCompact(516_733_981, { short: true })).toBe('516,7 млн');
		expect(fmt.tokensCompact(27_952_400_000, { short: true })).toBe('28 млрд');
	});
});
