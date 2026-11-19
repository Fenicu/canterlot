import { describe, expect, it } from 'vitest';
import { cacheHintText, cacheShare, idleText, costLine, costMoney, sessionCostLine, weekShare } from '#lib/format/cost.js';

const line = (over: Record<string, unknown> = {}) => ({
	model: 'claude-opus-5-5',
	agent: 'orchestrator',
	input_tokens: 2000,
	output_tokens: 1000,
	cache_read_tokens: 45000,
	cache_write_tokens: 0,
	cost_micro_usd: 620_000,
	cost_source: 'cli',
	...over
});

describe('costMoney', () => {
	it('uses the Russian format with a comma', () => {
		expect(costMoney(620_000)).toBe('≈ $0,62');
		expect(costMoney(1_234_560_000)).toBe('≈ $1 234,56');
	});
	it('says tiny amounts are below a cent', () => {
		expect(costMoney(4_000)).toBe('< $0,01');
		expect(costMoney(10_000)).toBe('≈ $0,01');
	});
	it('tells free from unknown', () => {
		expect(costMoney(0)).toBe('бесплатно');
		expect(costMoney(null)).toBe('≈ $ —');
		expect(costMoney(undefined)).toBe('≈ $ —');
	});
});

describe('cacheShare', () => {
	it('is cache read over all input', () => {
		expect(cacheShare({ input_tokens: 5, cache_read_tokens: 91, cache_write_tokens: 4 })).toBe(91);
	});
	it('is hidden without input', () => {
		expect(cacheShare({ input_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 })).toBeNull();
	});
});

describe('weekShare', () => {
	it('shows permille as percent of the week', () => {
		expect(weekShare(14)).toBe('≈ 1,4% недели');
		expect(weekShare(30)).toBe('≈ 3% недели');
	});
	it('is hidden when absent or zero', () => {
		expect(weekShare(null)).toBeNull();
		expect(weekShare(undefined)).toBeNull();
		expect(weekShare(0)).toBeNull();
	});
});

describe('costLine', () => {
	it('puts the model, tokens, cache, money and week in one line', () => {
		expect(costLine([line()], { weekly_delta_permille: 14 })).toBe(
			'Opus 5.5 · 48 тыс. токенов (кэш 96%) · ≈ $0,62 · ≈ 1,4% недели'
		);
	});
	it('drops cache and week when they are unknown', () => {
		const text = costLine([line({ input_tokens: 0, cache_read_tokens: 0, output_tokens: 700 })], null);
		expect(text).toBe('Opus 5.5 · 700 токенов · ≈ $0,62');
	});
	it('lists the main model first and the side model after it', () => {
		const side = line({ model: 'claude-haiku-5-5', agent: 'Explore', input_tokens: 100, cache_read_tokens: 0, output_tokens: 50, cost_micro_usd: 10_000 });
		expect(costLine([side, line()], null)).toBe('Opus 5.5 · 48 тыс. токенов (кэш 96%) · ≈ $0,62 + Haiku 5.5 ≈ $0,01');
	});
	it('counts several side models', () => {
		const a = line({ model: 'claude-haiku-5-5', agent: 'Explore', cost_micro_usd: 10_000, input_tokens: 1, cache_read_tokens: 0, output_tokens: 1 });
		const b = line({ model: 'gpt-6-luna', agent: 'Plan', cost_micro_usd: 20_000, input_tokens: 1, cache_read_tokens: 0, output_tokens: 1 });
		expect(costLine([line(), a, b], null)).toContain('+ 2 модели ≈ $0,03');
	});
	it('marks an unpriced model as unknown, never as zero', () => {
		expect(costLine([line({ cost_micro_usd: null, cost_source: null })], null)).toContain('≈ $ —');
	});
	it('says free models are free', () => {
		expect(costLine([line({ cost_micro_usd: 0 })], null)).toContain('бесплатно');
	});
	it('merges the same model run by several agents', () => {
		const text = costLine([line(), line({ agent: 'Explore', input_tokens: 0, cache_read_tokens: 0, output_tokens: 2000, cost_micro_usd: 380_000 })], null);
		expect(text).toBe('Opus 5.5 · 50 тыс. токенов (кэш 96%) · ≈ $1,00');
	});
	it('is empty without usage', () => {
		expect(costLine([], null)).toBe('');
	});
});

describe('sessionCostLine', () => {
	it('sums the session and its delegates', () => {
		expect(sessionCostLine({ cost_micro_usd: 1_000_000, unknown_cost_turns: 0 }, { cost_micro_usd: 500_000, unknown_cost_turns: 0 })).toEqual({
			money: '≈ $1,50 за сессию с делегатами',
			unknown: null
		});
	});
	it('counts turns without a price', () => {
		expect(sessionCostLine({ cost_micro_usd: 0, unknown_cost_turns: 1 }, { cost_micro_usd: 0, unknown_cost_turns: 1 }).unknown).toBe('2 хода без цены');
		expect(sessionCostLine({ cost_micro_usd: 0, unknown_cost_turns: 1 }, { cost_micro_usd: 0, unknown_cost_turns: 0 }).unknown).toBe('1 ход без цены');
		expect(sessionCostLine({ cost_micro_usd: 0, unknown_cost_turns: 5 }, { cost_micro_usd: 0, unknown_cost_turns: 0 }).unknown).toBe('5 ходов без цены');
	});
});

describe('cacheHintText', () => {
	it('names the likely expiry with the idle time', () => {
		expect(cacheHintText({ kind: 'expired', idle_minutes: 12 })).toBe('кэш, вероятно, истёк · простой 12 мин');
		expect(cacheHintText({ kind: 'expired', idle_minutes: 60 })).toBe('кэш, вероятно, истёк · простой 1 ч');
		expect(cacheHintText({ kind: 'expired', idle_minutes: 125 })).toBe('кэш, вероятно, истёк · простой 2 ч 5 мин');
		expect(cacheHintText({ kind: 'expired' })).toBe('кэш, вероятно, истёк');
	});
	it('names a model change, a compaction and a plain rebuild', () => {
		expect(cacheHintText({ kind: 'model_changed' })).toBe('смена модели — кэш заново');
		expect(cacheHintText({ kind: 'compaction' })).toBe('после сжатия контекста');
		expect(cacheHintText({ kind: 'rebuilt' })).toBe('кэш пересобран');
	});
	it('has no hint without data', () => {
		expect(cacheHintText(null)).toBeNull();
		expect(cacheHintText(undefined)).toBeNull();
	});
	it('formats idle time', () => {
		expect(idleText(5)).toBe('5 мин');
		expect(idleText(59)).toBe('59 мин');
		expect(idleText(61)).toBe('1 ч 1 мин');
	});
});
