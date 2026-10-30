import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';

const AT = '2026-10-28T12:00:00.000Z';

export interface MockupSessions {
	canterlot: string;
	review: string;
	books: string;
	machine: string;
	pinned: string;
	plain: string;
}

export function seedMockupSessions(api: FakeHubApi): MockupSessions {
	const canterlot = api.id();
	const review = api.id();
	const books = api.id();
	const machine = api.id();
	api.groups = [
		{ id: canterlot, name: 'canterlot', position: 0, created_at: AT },
		{ id: review, name: 'На проверку', position: 1, created_at: AT },
		{ id: books, name: 'bookshelf', position: 2, created_at: AT }
	];
	const common = { machine: { id: machine, name: 'workstation' } };
	const pinned = seedConversation(api, {
		...common,
		title: 'Восстановление потока',
		pinned: true,
		color: 'accent',
		unread_count: 1,
		model: 'opus-5.5',
		harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
		activity: { state: 'working', evidence: { phase: 'generating' }, since: AT }
	});
	seedConversation(api, {
		...common,
		title: 'Очередь сообщений',
		pinned: true,
		color: 'blue',
		model: 'gpt-6.1-sol',
		harness: { id: 'codex', title: 'Codex', provider: 'codex' },
		last_activity_at: '2026-10-28T19:14:00.000Z',
		activity: { state: 'working', evidence: { phase: 'running_tool', tool_name: 'cargo test', phase_since: '2026-10-28T19:10:00.000Z' }, since: AT }
	});
	seedConversation(api, {
		...common,
		title: 'Граница snapshot / replay',
		groups: [canterlot],
		color: 'amber',
		unread_count: 2,
		model: 'opus-5.5',
		harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
		activity: { state: 'waiting_for_user', evidence: { question: '2 варианта' }, since: AT }
	});
	seedConversation(api, {
		...common,
		title: 'Восстановление воркера',
		groups: [canterlot],
		color: 'neutral',
		model: 'opus-5.5',
		machine: { id: machine, name: 'mini' },
		harness: { id: 'claude', title: 'Claude Code', provider: 'claude' },
		activity: { state: 'quota_paused', evidence: { reason: 'weekly_reserve' }, since: AT }
	});
	seedConversation(api, {
		...common,
		title: 'Сборка адаптера',
		groups: [canterlot],
		color: 'blue',
		model: 'gpt-6.1-sol',
		machine: { id: machine, name: 'mini' },
		harness: { id: 'codex', title: 'Codex', provider: 'codex' },
		activity: { state: 'stalled', evidence: { last_event_at: AT }, since: AT }
	});
	const plain = seedConversation(api, {
		...common,
		title: 'Повторная доставка ввода',
		groups: [review],
		color: 'neutral',
		model: 'gpt-6.1-sol',
		harness: { id: 'codex', title: 'Codex', provider: 'codex' },
		activity: { state: 'stopped', evidence: { outcome: 'stopped' }, since: AT }
	});
	for (const title of ['Поиск по библиотеке', 'Каталог', 'Обложка', 'Глава']) {
		seedConversation(api, {
			...common,
			title,
			groups: [books],
			color: 'neutral',
			model: 'gpt-6.1-sol',
			machine: { id: machine, name: 'mini' },
			harness: { id: 'codex', title: 'Codex', provider: 'codex' },
			activity: { state: 'stopped', evidence: {}, since: AT }
		});
	}
	api.counts = {
		all: 10,
		pinned: 2,
		archived: 0,
		groups: [
			{ id: canterlot, count: 3 },
			{ id: review, count: 1 },
			{ id: books, count: 4 }
		]
	};
	const observed = new Date(Date.now() - 60_000).toISOString();
	const limits: ProviderLimits[] = [
		{
			provider: 'claude',
			weekly_reserve_active: false,
			five_hour: { used_permille: 340, resets_at: new Date(Date.now() + 3_600_000).toISOString(), observed_at: observed },
			weekly: { used_permille: 960, resets_at: new Date(Date.now() + 86_400_000).toISOString(), observed_at: observed }
		},
		{
			provider: 'codex',
			weekly_reserve_active: false,
			five_hour: { used_permille: 90, resets_at: new Date(Date.now() + 3_600_000).toISOString(), observed_at: observed },
			weekly: { used_permille: 150, resets_at: new Date(Date.now() + 86_400_000).toISOString(), observed_at: observed }
		}
	];
	api.limits = limits;
	const record: MachineRecord = {
		id: machine,
		name: 'workstation',
		availability: 'ready',
		os: 'linux',
		warnings: [],
		metrics: null,
		capabilities: ['pilot_v1'],
		allowed_roots: ['/srv'],
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [{ id: 'opus', title: 'opus-5.5', efforts: ['medium', 'high'] }],
				permission_modes: ['default']
			}
		]
	};
	api.machines.set(machine, record);
	const degraded = api.id();
	api.machines.set(degraded, { ...record, id: degraded, name: 'mini', availability: 'degraded', capabilities: [] });
	return { canterlot, review, books, machine, pinned, plain };
}
