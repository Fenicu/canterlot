import { describe, expect, it } from 'vitest';
import type { components } from '#lib/api/generated/openapi.js';
import {
	blockText,
	deleteOutcomeText,
	profileLines,
	proposalRefusalText,
	splitWindows,
	windowCost,
	windowCostLine,
	windowDeadline,
	windowEndReason,
	windowReplacement,
	windowStateText,
	windowStateWord
} from '#lib/screens/machines/cloudModel.js';
import {
	cloudBannerText,
	cloudCardLifetime,
	cloudOutcome,
	cloudWindowView
} from '#lib/requests/cloudWindowView.js';

type Window = components['schemas']['CloudWindowView'];

const NOW = Date.parse('2026-11-23T12:00:00.000Z');

function window(patch: Partial<Window> = {}): Window {
	return {
		id: '00000000-0000-4000-8000-0000000000c1',
		name: 'canterlot-runner-1',
		state: 'ready',
		trigger: 'card',
		instance_id: 'fhmfakeinstance0777',
		public_ip: '203.0.113.7',
		preemptible: true,
		replaces_window_id: null,
		compute_kopecks_per_hour: 2_685,
		disk_kopecks_per_hour: 160,
		price_hourly_kopecks: 2_845,
		reservation_kopecks: 11_380,
		final_cost_kopecks: null,
		cost_so_far_kopecks: 2_845,
		month: '2026-11',
		created_at: new Date(NOW - 40 * 60_000).toISOString(),
		ready_at: new Date(NOW - 35 * 60_000).toISOString(),
		hard_cap_at: new Date(NOW + 80 * 60_000).toISOString(),
		stopped_at: null,
		disk_gone_at: null,
		delete_requested_at: null,
		delete_confirmed_at: null,
		delete_reason: null,
		last_error: null,
		...patch
	};
}

describe('cloud windows on the machines screen', () => {
	it('takes the live window apart from the ended ones', () => {
		const live = window();
		const ended = window({ id: '2', state: 'ended', final_cost_kopecks: 5_690 });
		const split = splitWindows([live, ended, window({ id: '3', state: 'failed' })]);
		expect(split.live?.id).toBe(live.id);
		expect(split.ended.map((item) => item.id)).toEqual(['2', '3']);
	});

	it('shows what is already spent on a live window and the final cost of an ended one', () => {
		expect(windowCost(window())).toBe(2_845);
		expect(windowCost(window({ final_cost_kopecks: 5_690, cost_so_far_kopecks: 0 }))).toBe(5_690);
		expect(windowCostLine(window())).toBe('Расход 28,45 ₽ из резерва 113,80 ₽');
		expect(windowCostLine(window({ state: 'ended', final_cost_kopecks: 5_690 }))).toBe('Итог 56,90 ₽');
	});

	it('names a deadline in the past instead of a negative time', () => {
		expect(windowDeadline(window(), NOW)).toBe('до предела 1 ч 20 мин');
		expect(windowDeadline(window({ hard_cap_at: new Date(NOW - 60_000).toISOString() }), NOW)).toBe(
			'предел прошёл, окно дорабатывает и удаляется'
		);
		expect(windowDeadline(window({ hard_cap_at: null }), NOW)).toBeNull();
	});

	it('turns hub states into owner words', () => {
		expect(windowStateWord('create_unknown')).toBe('Создание без ответа');
		expect(windowStateWord('draining')).toBe('Завершается');
		expect(windowStateWord('ended')).toBe('Завершено');
		expect(windowStateWord('whatever')).toBe('Состояние неизвестно');
	});

	it('explains the provider block and the delete outcome without codes', () => {
		expect(blockText('billing_blocked')).toBe(
			'Провайдер остановил создание окон (код billing_blocked)'
		);
		expect(blockText(null)).toBeNull();
		expect(deleteOutcomeText('deleting')).toBe('Удаляю машину. Оплата идёт до подтверждения удаления');
		expect(deleteOutcomeText('not_yet')).toContain('Удаление повторится');
		expect(proposalRefusalText('live_window_exists')).toContain('одно');
		expect(proposalRefusalText('provider_unavailable')).toContain('Yandex Cloud');
	});

	it('names a preemption and a broken machine in owner words', () => {
		expect(windowEndReason('preempted')).toBe('прервана Яндексом');
		expect(windowEndReason('vm_broken')).toBe('машина сломалась');
		expect(windowEndReason('owner')).toBeNull();
		expect(windowStateText(window({ state: 'ended', delete_reason: 'preempted' }))).toBe(
			'Завершено · прервана Яндексом'
		);
		expect(windowStateText(window({ state: 'deleting', delete_reason: 'vm_broken' }))).toBe(
			'Удаляется · машина сломалась'
		);
	});

	it('ties a replacement window to the parent it replaces', () => {
		const parent = window({ id: 'parent', state: 'ended', delete_reason: 'preempted' });
		const replacement = window({ id: 'child', replaces_window_id: parent.id });
		const line = windowReplacement(replacement, [parent, replacement], NOW);
		expect(line?.parentId).toBe(parent.id);
		expect(line?.text.startsWith('замена окна')).toBe(true);
		expect(windowReplacement(window(), [parent], NOW)).toBeNull();
		expect(windowReplacement(replacement, [replacement], NOW)).toEqual({
			text: 'замена окна',
			parentId: null
		});
	});
});

describe('cloud proposal card', () => {
	const payload = {
		source: 'agent',
		reason: 'Собрать релиз',
		hours: 2,
		hard_cap_minutes: 120,
		hourly_kopecks: 2_845,
		reservation_kopecks: 11_380,
		remaining_kopecks: 20_000,
		over_budget: false,
		expires_at: new Date(NOW + 10 * 60_000).toISOString(),
		profiles: []
	};

	it('reads hours and money from the card', () => {
		const view = cloudWindowView(payload);
		expect(view.title).toBe('Облачное окно на 2 ч');
		expect(view.hourlyText).toBe('28,45 ₽');
		expect(view.reservationText).toBe('113,80 ₽');
		expect(view.remainingText).toBe('200,00 ₽');
		expect(view.overBudget).toBe(false);
		expect(view.sourceText).toContain('агент');
	});

	it('keeps the over-budget flag the hub set', () => {
		const view = cloudWindowView({ ...payload, over_budget: true, remaining_kopecks: 4_000 });
		expect(view.overBudget).toBe(true);
		expect(cloudBannerText({ ...payload, over_budget: true })).toContain('сверх бюджета');
	});

	it('counts down the half hour the card lives', () => {
		expect(cloudCardLifetime(payload.expires_at, NOW)).toBe('Карточка живёт ещё 10 минут');
		expect(cloudCardLifetime(new Date(NOW + 60_000).toISOString(), NOW)).toBe(
			'Карточка живёт ещё 1 минуту'
		);
		expect(cloudCardLifetime(new Date(NOW - 1).toISOString(), NOW)).toBe('Срок карточки истёк');
	});

	it('tells price changes from a real refusal', () => {
		expect(cloudOutcome('card_outdated').word).toBe('Выпущена новая карточка');
		expect(cloudOutcome('expired').tone).toBe('quiet');
		expect(cloudOutcome('provider_blocked').word).toBe('Провайдер заблокирован');
		expect(cloudOutcome('provider_blocked').note).toContain('Провайдер остановил');
		expect(cloudOutcome('over_budget').word).toBe('Не хватает бюджета');
		expect(cloudOutcome('whatever').word).toBe('Не применено');
	});

	it('reads a profile as size, disk and an approximate hour', () => {
		expect(
			profileLines([
				{
					cores: 32,
					memory_gb: 64,
					disk_gb: 80,
					compute_kopecks_per_hour: 1_646,
					disk_kopecks_per_hour: 160
				}
			])
		).toEqual([{ key: '32-64-80', text: '32 vCPU · 64 ГБ · SSD 80 ГБ · ≈18,06 ₽/ч' }]);
	});
});
