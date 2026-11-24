// Облачные окна в фиктивном хабе: список, «Удалить сейчас», бюджет, блок провайдера и
// карточки запуска. Служат и экрану машин, и настройкам, и баннеру оболочки.
import type { Page } from '@playwright/test';
import type { components } from '../../src/lib/api/generated/openapi.js';
import type { FakeHubApi } from './api';
import { seedConversation } from './seed';

export type FakeCloudWindow = components['schemas']['CloudWindowView'];
export type FakeCloudBudget = components['schemas']['BudgetView'];
export type FakeCloudCard = components['schemas']['RequestView'];

const now = (): number => Date.now();
const iso = (ms: number): string => new Date(ms).toISOString();

const DEFAULT_HOURLY = 2_845;

function errorBody(code: string, message: string, details?: Record<string, unknown>): unknown {
	return {
		error: {
			code,
			message,
			scope: 'hub_db',
			retry: 'never',
			correlation_id: '00000000-0000-4000-8000-0000000000c4',
			...(details ? { details } : {})
		}
	};
}

function headers(): Record<string, string> {
	return { 'X-Server-Time-Ms': String(now()) };
}

/** Живое окно: готово, работает 42 минуты, предел — через полтора часа. */
export function seedCloudWindow(api: FakeHubApi, patch: Partial<FakeCloudWindow> = {}): FakeCloudWindow {
	const id = patch.id ?? api.id();
	const created = now() - 42 * 60_000;
	const window: FakeCloudWindow = {
		id,
		name: patch.name ?? `canterlot-runner-${id}`,
		state: 'ready',
		trigger: 'card',
		instance_id: 'fhmfakeinstance0777',
		public_ip: '203.0.113.7',
		preemptible: true,
		replaces_window_id: null,
		compute_kopecks_per_hour: DEFAULT_HOURLY - 160,
		disk_kopecks_per_hour: 160,
		price_hourly_kopecks: DEFAULT_HOURLY,
		reservation_kopecks: 11_380,
		final_cost_kopecks: null,
		cost_so_far_kopecks: DEFAULT_HOURLY,
		month: new Date().toISOString().slice(0, 7),
		created_at: iso(created),
		ready_at: iso(created + 4 * 60_000),
		hard_cap_at: iso(created + 120 * 60_000),
		stopped_at: null,
		disk_gone_at: null,
		delete_requested_at: null,
		delete_confirmed_at: null,
		delete_reason: null,
		last_error: null,
		...patch
	};
	api.cloudWindows.set(window.id, window);
	return window;
}

/** Законченное окно: итог и подтверждённое удаление. */
export function seedEndedWindow(api: FakeHubApi, patch: Partial<FakeCloudWindow> = {}): FakeCloudWindow {
	const at = now() - 26 * 3_600_000;
	return seedCloudWindow(api, {
		state: 'ended',
		created_at: iso(at),
		ready_at: iso(at + 5 * 60_000),
		hard_cap_at: iso(at + 120 * 60_000),
		delete_requested_at: iso(at + 118 * 60_000),
		delete_confirmed_at: iso(at + 119 * 60_000),
		final_cost_kopecks: 5_690,
		cost_so_far_kopecks: 5_690,
		...patch
	});
}

export interface CloudCardSeed {
	reason?: string;
	hours?: number;
	source?: string;
	over_budget?: boolean;
	remaining_kopecks?: number;
	conversationId?: string;
}

/**
 * Карточка `cloud_window` в скрытом служебном разговоре: разговор не попадает в список
 * сессий, а карточка — в `cloud_cards` снимка списка.
 */
export function seedCloudCard(
	api: FakeHubApi,
	patch: CloudCardSeed = {}
): { conversation: string; request: FakeCloudCard } {
	const conversation =
		patch.conversationId ??
		seedConversation(api, {
			title: 'Облачные окна',
			harness: { id: 'cloud', title: 'Облако', provider: 'cloud' },
			workdir: '/srv/canterlot-runner'
		});
	// Настоящий хаб скрывает служебный разговор из списка (`harness <> 'cloud'`).
	if (api.list.delete(conversation)) {
		api.counts = { ...api.counts, all: Math.max(0, api.counts.all - 1) };
	}
	const hours = patch.hours ?? 2;
	const remaining = patch.remaining_kopecks ?? api.cloudBudget.remaining_kopecks;
	const reservation = (hours + 2) * DEFAULT_HOURLY;
	const payload = {
		source: patch.source ?? 'agent',
		reason: patch.reason ?? 'Собрать релиз и прогнать гейт на облачном раннере',
		hours,
		hard_cap_minutes: hours * 60,
		profiles: api.cloudBudget.profiles,
		hourly_kopecks: DEFAULT_HOURLY,
		reservation_kopecks: reservation,
		remaining_kopecks: remaining,
		over_budget: patch.over_budget ?? reservation > remaining,
		expires_at: iso(now() + 24 * 60_000)
	};
	const id = api.id();
	const request: FakeCloudCard = {
		id,
		conversation_id: conversation,
		execution_id: api.id(),
		generation: 0,
		kind: 'cloud_window',
		native_request_id: `cloud_window:${id}`,
		request_payload: payload,
		state: 'open',
		created_at: iso(now()),
		updated_at: iso(now()),
		winner_decision_id: null
	};
	api.cloudCards.set(id, request);
	api.hub.append(`conversation:${conversation}`, {
		kind: 'request_opened',
		conversation_id: conversation,
		payload: { request_id: id, kind: 'cloud_window', payload }
	});
	api.hub.flush();
	return { conversation, request };
}

export interface CloudSeed {
	/** `false` — хаба без `[runners.cloud]`: списки пусты, форма скрыта. */
	configured?: boolean;
	budget?: Partial<FakeCloudBudget>;
	/** Код блока провайдера, например `no_paid`. */
	block?: string | null;
	/** Живое окно: `false` — его нет. */
	live?: false | Partial<FakeCloudWindow>;
	/** Сколько законченных окон положить в список. */
	ended?: number;
	/** Карточка запуска: `false` — её нет. */
	card?: false | CloudCardSeed;
}

/** Всё облако разом: бюджет, блок, живое окно, история и открытая карточка. */
export function seedCloud(
	api: FakeHubApi,
	opts: CloudSeed = {}
): { conversation?: string; request?: FakeCloudCard } {
	const configured = opts.configured ?? true;
	api.cloudBudget = {
		...api.cloudBudget,
		...(opts.budget ?? {}),
		configured,
		profiles: configured ? api.cloudBudget.profiles : [],
		provider_block_code: opts.block ?? null,
		provider_block_at: opts.block ? iso(now() - 35 * 60_000) : null
	};
	api.cloudBudget.remaining_kopecks =
		api.cloudBudget.monthly_budget_kopecks - api.cloudBudget.spent_kopecks;
	if (opts.live !== false) seedCloudWindow(api, opts.live ?? {});
	for (let index = 0; index < (opts.ended ?? 0); index += 1) {
		seedEndedWindow(api, { created_at: iso(now() - (26 + index * 30) * 3_600_000) });
	}
	if (opts.card === false) return {};
	const seeded = seedCloudCard(api, opts.card ?? {});
	return { conversation: seeded.conversation, request: seeded.request };
}

/** Маршруты облака для обвязки: те же адреса и коды, что у хаба. */
export async function installCloudRoutes(
	page: Page,
	api: FakeHubApi
): Promise<void> {
	await page.route(/\/api\/v1\/cloud\/windows$/, async (route) => {
		if (route.request().method() !== 'GET') return route.fallback();
		const live = [...api.cloudWindows.values()]
			.filter((window) => !['ended', 'failed', 'gone'].includes(window.state))
			.sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));
		const ended = [...api.cloudWindows.values()]
			.filter((window) => ['ended', 'failed', 'gone'].includes(window.state))
			.sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at))
			.slice(0, 20);
		await route.fulfill({
			headers: headers(),
			json: {
				items: [...live, ...ended],
				provider_block_code: api.cloudBudget.provider_block_code,
				provider_block_at: api.cloudBudget.provider_block_at,
				configured: api.cloudBudget.configured
			}
		});
	});

	await page.route(/\/api\/v1\/cloud\/windows\/[^/]+\/delete$/, async (route) => {
		if (route.request().method() !== 'POST') return route.fallback();
		const key = route.request().headers()['idempotency-key'];
		if (!key) {
			await route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
			return;
		}
		const id = new URL(route.request().url()).pathname.split('/')[5] ?? '';
		const window = api.cloudWindows.get(id);
		if (!window) {
			await route.fulfill({ status: 404, headers: headers(), json: errorBody('not_found', 'Окно не найдено') });
			return;
		}
		api.cloudDeleteCalls.push({ id, key });
		if (['ended', 'failed', 'gone'].includes(window.state)) {
			await route.fulfill({ headers: headers(), json: { outcome: 'already_ended', window } });
			return;
		}
		const next: FakeCloudWindow = {
			...window,
			state: 'deleting',
			delete_requested_at: iso(now()),
			delete_reason: 'owner'
		};
		api.cloudWindows.set(id, next);
		await route.fulfill({ headers: headers(), json: { outcome: 'deleting', window: next } });
	});

	await page.route(/\/api\/v1\/cloud\/budget$/, async (route) => {
		const method = route.request().method();
		if (method === 'GET') {
			await route.fulfill({ headers: headers(), json: api.cloudBudget });
			return;
		}
		if (method !== 'PUT') return route.fallback();
		const key = route.request().headers()['idempotency-key'];
		if (!key) {
			await route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
			return;
		}
		const body = route.request().postDataJSON() as components['schemas']['Update'];
		api.cloudBudgetPuts.push({ body, key });
		if (body.base_revision !== api.cloudBudget.revision) {
			await route.fulfill({ status: 409, headers: headers(), json: errorBody('stale_revision', 'Настройки изменились') });
			return;
		}
		if (!Number.isInteger(body.monthly_budget_kopecks) || body.monthly_budget_kopecks < 0) {
			await route.fulfill({ status: 422, headers: headers(), json: errorBody('invalid_request', 'Некорректный бюджет') });
			return;
		}
		api.cloudBudget = {
			...api.cloudBudget,
			monthly_budget_kopecks: body.monthly_budget_kopecks,
			remaining_kopecks: body.monthly_budget_kopecks - api.cloudBudget.spent_kopecks,
			auto_mode: body.auto_mode,
			revision: api.cloudBudget.revision + 1
		};
		await route.fulfill({ headers: headers(), json: api.cloudBudget });
	});

	await page.route(/\/api\/v1\/cloud\/budget\/clear-block$/, async (route) => {
		if (route.request().method() !== 'POST') return route.fallback();
		const key = route.request().headers()['idempotency-key'];
		if (!key) {
			await route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
			return;
		}
		api.cloudBlockClears.push(key);
		const cleared = api.cloudBudget.provider_block_code !== null;
		api.cloudBudget = { ...api.cloudBudget, provider_block_code: null, provider_block_at: null };
		await route.fulfill({ headers: headers(), json: { cleared } });
	});

	await page.route(/\/api\/v1\/cloud\/proposals$/, async (route) => {
		if (route.request().method() !== 'POST') return route.fallback();
		const key = route.request().headers()['idempotency-key'];
		if (!key) {
			await route.fulfill({ status: 400, headers: headers(), json: errorBody('invalid_request', 'Нужен Idempotency-Key') });
			return;
		}
		const body = route.request().postDataJSON() as { hours?: number; reason?: string };
		const hours = body.hours ?? 0;
		api.cloudProposalCalls.push({ hours, reason: body.reason ?? '', key });
		if (!Number.isInteger(hours) || hours < 1 || hours > 4) {
			await route.fulfill({ status: 409, headers: headers(), json: errorBody('invalid_state', 'Некорректные часы', { reason: 'invalid_hours' }) });
			return;
		}
		const live = [...api.cloudWindows.values()].some((window) => !['ended', 'failed', 'gone'].includes(window.state));
		if (live) {
			await route.fulfill({ status: 409, headers: headers(), json: errorBody('invalid_state', 'Живое окно уже есть', { reason: 'live_window_exists' }) });
			return;
		}
		if (api.cloudCards.size > 0) {
			await route.fulfill({ status: 409, headers: headers(), json: errorBody('invalid_state', 'Карточка уже открыта', { reason: 'open_card_exists' }) });
			return;
		}
		const maxHourly = Math.max(0, ...api.cloudBudget.profiles.map((profile) => profile.compute_kopecks_per_hour + profile.disk_kopecks_per_hour));
		const reservation = (hours + 2) * maxHourly;
		const over = reservation > api.cloudBudget.remaining_kopecks;
		const { request } = seedCloudCard(api, {
			hours,
			reason: body.reason ?? '',
			source: 'owner',
			over_budget: over
		});
		await route.fulfill({
			headers: headers(),
			json: {
				request_id: request.id,
				reservation_kopecks: reservation,
				over_budget: over,
				expires_at: (request.request_payload as { expires_at: string }).expires_at
			}
		});
	});
}
