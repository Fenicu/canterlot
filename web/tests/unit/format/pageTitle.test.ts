import { describe, expect, it } from 'vitest';
import { laneTabTitle, sessionTabTitle, TAB_TITLE_LIMIT, trimTabTitle } from '#lib/format/pageTitle.js';

describe('pageTitle', () => {
	it('uses_the_session_title_in_the_tab', () => {
		expect(sessionTabTitle('Починить дедупликацию')).toBe('Починить дедупликацию · Canterlot');
	});

	it('falls_back_to_the_header_placeholder_for_an_empty_title', () => {
		expect(trimTabTitle('')).toBe('Сессия');
		expect(trimTabTitle(null)).toBe('Сессия');
		expect(sessionTabTitle('   ')).toBe('Сессия · Canterlot');
	});

	it('trims_a_long_title_to_the_limit_with_an_ellipsis', () => {
		const long = 'Сессия '.repeat(20).trim();
		const trimmed = trimTabTitle(long);
		expect(Array.from(trimmed)).toHaveLength(TAB_TITLE_LIMIT);
		expect(trimmed.endsWith('…')).toBe(true);
		expect(long.startsWith(trimmed.slice(0, -1))).toBe(true);
	});

	it('puts_the_lane_prefix_before_the_shortened_title', () => {
		expect(laneTabTitle('Поиск дедупликации')).toBe('Субагент · Поиск дедупликации · Canterlot');
		expect(laneTabTitle(undefined)).toBe('Субагент · Сессия · Canterlot');
	});
});
