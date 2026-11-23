import { describe, expect, it } from 'vitest';
import { kopecksInput, parseRubles, rubles } from '#lib/format/money.js';

describe('rubles', () => {
	it('writes kopecks as rubles with two decimals', () => {
		expect(rubles(0)).toBe('0,00 ₽');
		expect(rubles(5)).toBe('0,05 ₽');
		expect(rubles(123_450)).toBe('1 234,50 ₽');
		expect(rubles(100_000_000)).toBe('1 000 000,00 ₽');
	});

	it('keeps the sign of a negative balance', () => {
		expect(rubles(-123_450)).toBe('-1 234,50 ₽');
		expect(rubles(-5)).toBe('-0,05 ₽');
	});

	it('does not invent a number when there is none', () => {
		expect(rubles(Number.NaN)).toBe('—');
		expect(rubles(Number.POSITIVE_INFINITY)).toBe('—');
	});
});

describe('parseRubles', () => {
	it('reads the forms an owner types', () => {
		expect(parseRubles('0')).toBe(0);
		expect(parseRubles('12')).toBe(1_200);
		expect(parseRubles('1 234')).toBe(123_400);
		expect(parseRubles('1 234,50')).toBe(123_450);
		expect(parseRubles('1234.5')).toBe(123_450);
		expect(parseRubles(' 5 000,05 ₽ ')).toBe(500_005);
	});

	it('refuses anything it would have to guess', () => {
		expect(parseRubles('')).toBeNull();
		expect(parseRubles('   ')).toBeNull();
		expect(parseRubles('abc')).toBeNull();
		expect(parseRubles('-5')).toBeNull();
		expect(parseRubles('1,234')).toBeNull();
		expect(parseRubles('1.2.3')).toBeNull();
		expect(parseRubles('1 234,567')).toBeNull();
		expect(parseRubles('12,3,4')).toBeNull();
	});
});

describe('kopecksInput', () => {
	it('round-trips through the field', () => {
		expect(kopecksInput(123_450)).toBe('1 234,50');
		expect(parseRubles(kopecksInput(123_450))).toBe(123_450);
		expect(kopecksInput(0)).toBe('0,00');
	});
});
