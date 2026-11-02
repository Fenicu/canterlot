import { describe, expect, it } from 'vitest';
import {
	compactionLine,
	compactionRunningLine,
	contextBadge,
	contextDetails,
	contextPercent,
	contextTone
} from '#lib/timeline/compaction.js';

describe('compaction line', () => {
	it('names the trigger and the reported size', () => {
		expect(compactionLine({ trigger: 'auto', ok: true, pre_tokens: 980_000 })).toBe(
			'Контекст сжат автоматически, было 980 тыс. токенов'
		);
		expect(compactionLine({ trigger: 'manual', ok: true, pre_tokens: 980_000 })).toBe(
			'Контекст сжат вручную, было 980 тыс. токенов'
		);
	});
	it('has no number when the CLI gave none', () => {
		expect(compactionLine({ trigger: 'auto', ok: true, pre_tokens: null })).toBe(
			'Контекст сжат автоматически'
		);
		expect(compactionLine({ trigger: 'auto', ok: true })).toBe('Контекст сжат автоматически');
	});
	it('reports failure in words', () => {
		expect(compactionLine({ trigger: 'auto', ok: false, pre_tokens: 5 })).toBe(
			'Сжатие контекста не удалось'
		);
	});
	it('running line is a service phrase', () => {
		expect(compactionRunningLine()).toBe('Сжимает контекст');
	});
});

describe('context fill', () => {
	it('shows fill, not remainder', () => {
		expect(contextPercent(640_000, 1_000_000)).toBe(64);
		expect(contextBadge(640_000, 1_000_000)).toBe('Контекст 64%');
	});
	it('never invents a percent without a window', () => {
		expect(contextPercent(640_000, null)).toBeNull();
		expect(contextPercent(640_000, 0)).toBeNull();
		expect(contextBadge(640_000, null)).toBe('Контекст 640 тыс.');
		expect(contextDetails(640_000, null)).toBe('640 тыс. токенов');
	});
	it('details name used of the window', () => {
		expect(contextDetails(640_000, 1_000_000)).toBe('640 тыс. из 1 млн токенов');
	});
	it('tones switch at 70 and 90', () => {
		expect(contextTone(69)).toBe('accent');
		expect(contextTone(70)).toBe('warning');
		expect(contextTone(89)).toBe('warning');
		expect(contextTone(90)).toBe('danger');
		expect(contextTone(null)).toBe('accent');
	});
});
