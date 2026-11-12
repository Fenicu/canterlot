import type { MachineRecord } from '../../src/lib/stores/machines.svelte';
import type { ProviderLimits } from '../../src/lib/stores/limits';
import type { FakeHubApi } from '../fake-hub/api';
import { seedConversation } from '../fake-hub/seed';

/** Список как в утверждённом макете строк-чатов: вендоры, состояния, превью и делегаты. */
export interface ChatSessions {
	canterlot: string;
	work: string;
	home: string;
	orchestrator: string;
	kids: [string, string];
	waiting: string;
	pinned: string;
	stalled: string;
	paused: string;
	machine: string;
}

const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };
const CODEX = { id: 'codex', title: 'Codex', provider: 'codex' };
const OPENCODE = { id: 'opencode', title: 'OpenCode', provider: 'opencode' };
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
const ahead = (hours: number) => new Date(Date.now() + hours * 3_600_000).toISOString();

export const ORCHESTRATOR_PREVIEW = 'Выпуск 1.29.0 на проде, проверила отказ по «gemini» и вернула старый маршрут';
export const PINNED_PREVIEW = 'Готово: тесты зелёные, коммит a3f9c1e';

export function seedChatSessions(api: FakeHubApi, opts: { machines?: number } = {}): ChatSessions {
	const canterlot = api.id();
	const work = api.id();
	const home = api.id();
	const machine = api.id();
	api.groups = [
		{ id: canterlot, name: 'Canterlot', position: 0, icon: 'code', created_at: ago(600) },
		{ id: work, name: 'Работа', position: 1, icon: 'briefcase', created_at: ago(600) },
		{ id: home, name: 'Егорчик', position: 2, icon: 'home', created_at: ago(600) }
	];
	const common = { machine: { id: machine, name: 'dev-vm' } };
	const orchestrator = seedConversation(api, {
		...common,
		title: 'Кантерлот',
		groups: [canterlot],
		harness: CLAUDE,
		model: 'opus[1m]',
		model_resolved: 'claude-opus-5-5',
		last_activity_at: ago(2),
		preview: ORCHESTRATOR_PREVIEW,
		activity: { state: 'idle', evidence: { kind: 'idle', last_turn_at: ago(2) }, since: ago(2) }
	});
	const kid = (title: string, minutes: number, activity: Record<string, unknown>) =>
		seedConversation(api, { ...common, title, parent_id: orchestrator, spawn_depth: 1, harness: CLAUDE, model: 'opus[1m]', model_resolved: 'claude-opus-5-5', last_activity_at: ago(minutes), working_ms: 240_000, activity: activity as never });
	const kids: [string, string] = [
		kid('Вход 1: ядро хаба', 4, { state: 'working', evidence: { kind: 'working', phase: 'generating' }, since: ago(5) }),
		kid('MCP владельца для делегатов', 3, { state: 'working', evidence: { kind: 'working', phase: 'running_tool', summary: 'cargo nextest', phase_since: ago(4) }, since: ago(6) })
	];
	for (let index = 0; index < 4; index += 1) {
		api.pushPlanTask({ orchestrator_id: orchestrator, title: `Задача ${index + 1}`, state: index < 3 ? 'done' : 'in_progress', created_at: ago(300), updated_at: ago(10) });
	}
	const waiting = seedConversation(api, {
		...common,
		title: 'Склад: фильтр товаров',
		groups: [work],
		harness: CODEX,
		model: 'gpt-6.1-sol',
		unread_count: 2,
		last_activity_at: ago(70),
		activity: { state: 'waiting_for_user', evidence: { kind: 'waiting_for_user', question: '2 варианта' }, since: ago(70) }
	});
	const pinned = seedConversation(api, {
		...common,
		title: 'Бот расписания',
		groups: [home],
		pinned: true,
		harness: OPENCODE,
		model: 'deepseek/deepseek-v4.1',
		last_activity_at: ago(110),
		preview: PINNED_PREVIEW,
		activity: { state: 'idle', evidence: { kind: 'idle', last_turn_at: ago(110) }, since: ago(110) }
	});
	const stalled = seedConversation(api, {
		...common,
		title: 'Разбор утечки памяти',
		groups: [home],
		harness: OPENCODE,
		model: 'meta/muse-spark',
		last_activity_at: ago(140),
		activity: { state: 'stalled', evidence: { kind: 'stalled', last_event_at: ago(12) }, since: ago(12) }
	});
	const paused = seedConversation(api, {
		...common,
		title: 'Облегчение бандла',
		groups: [home],
		harness: OPENCODE,
		model: 'xiaomi/mimo-v2-flash',
		last_activity_at: ago(60 * 26),
		activity: { state: 'quota_paused', evidence: { kind: 'quota_paused', reason: 'weekly_reserve' }, since: ago(60) }
	});
	api.counts = {
		all: 5,
		pinned: 1,
		archived: 0,
		trashed: 0,
		groups: [
			{ id: canterlot, count: 1 },
			{ id: work, count: 1 },
			{ id: home, count: 3 }
		]
	};
	const observed = ago(1);
	const limits: ProviderLimits[] = [
		{ provider: 'claude', weekly_reserve_active: false, five_hour: { used_permille: 120, resets_at: ahead(3), observed_at: observed }, weekly: { used_permille: 250, resets_at: ahead(100), observed_at: observed } },
		{ provider: 'codex', weekly_reserve_active: false, five_hour: { used_permille: 0, resets_at: ahead(4), observed_at: observed }, weekly: { used_permille: 900, resets_at: ahead(80), observed_at: observed } },
		{ provider: 'opencode', weekly_reserve_active: false, five_hour_absent: true, weekly: { used_permille: 200, resets_at: ahead(90), observed_at: observed }, monthly: { used_permille: 460, resets_at: ahead(300), observed_at: observed } }
	];
	api.limits = limits;
	const record: MachineRecord = {
		id: machine,
		name: 'dev-vm',
		availability: 'ready',
		os: 'linux',
		warnings: [],
		metrics: null,
		capabilities: ['pilot_v1'],
		allowed_roots: ['/srv'],
		harnesses: [
			{ id: 'claude_code', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus[1m]', title: 'Opus 5.5 1M', efforts: ['high'] }], permission_modes: ['owner', 'ask', 'bypass'] },
			{ id: 'opencode', title: 'OpenCode', provider: 'opencode', models: [{ id: 'deepseek/deepseek-v4.1', title: 'DeepSeek V4.1', efforts: ['default'] }], permission_modes: ['owner', 'ask', 'bypass'] }
		]
	};
	api.machines.set(machine, record);
	for (let index = 1; index < (opts.machines ?? 1); index += 1) {
		const other = api.id();
		api.machines.set(other, { ...record, id: other, name: `mini-${index}` });
	}
	return { canterlot, work, home, orchestrator, kids, waiting, pinned, stalled, paused, machine };
}
