import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { sha256Hex } from '../../src/lib/stores/blocks';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const MIN = 60_000;
const TURNS = 40;
const PHASES = [
	['Read', 'README.md'],
	['Grep', 'fn main'],
	['Bash', 'cargo test'],
	['Edit', 'src/lib.rs']
] as const;
// Возраст субагентов на старте: за окно замера запись времени не переходит в следующий разряд.
const STEADY = [12_500, 24_500, 72_500, 17_000];
// Возраст рядом с переходами 9 с, 59 с и 1 мин 9 с.
const CROSSING = [6_500, 55_500, 124_500, 7_000];

const at = (msAgo: number): string => new Date(Date.now() - msAgo).toISOString();

function block(api: FakeHubApi, seq: number, patch: Partial<HistoryBlock> & { content: string }): HistoryBlock {
	const id = api.id();
	api.blocks.set(id, { text: patch.content, rev: 1, state: patch.state ?? 'final' });
	return {
		kind: 'block', id, block_kind: 'text', content_bytes: patch.content.length, content_truncated: false,
		// История лежит раньше живых событий: те получают номера с 1.
		created_seq: String(seq - 1000), created_at: at(0), rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5',
		tool_name: null, summary: null, parent_block_id: null, input_command_id: null, line_count: 1, ...patch
	};
}

/** Длинная лента прошлых ходов и живой ход с четырьмя работающими субагентами. */
async function boot(page: Page, ages: number[]) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	seedMockupSessions(api);
	const id = seedConversation(api, { title: 'jitter', model: 'opus-5.5' });
	const items: HistoryBlock[] = [];
	let seq = 1;
	for (let turn = 0; turn < TURNS; turn++) {
		const text = `ход ${turn}: ${'разбираю модуль и проверяю результат. '.repeat(3 + (turn % 4))}`;
		items.push(block(api, seq++, { content: text, input_command_id: api.id(), created_at: at(90 * MIN - turn * 3 * MIN) }));
	}
	const run = api.id();
	const calls: HistoryBlock[] = [];
	ages.forEach((age, i) => {
		const started = at(age);
		const call = block(api, seq++, {
			content: `задача ${i}`, block_kind: 'tool_call', tool_name: 'Agent', summary: `задача ${i}: разобрать модуль`, agent: 'orchestrator',
			state: 'streaming', input_command_id: run, created_at: started, started_at: started
		});
		calls.push(call);
		items.push(call);
		const [tool, arg] = PHASES[i % PHASES.length]!;
		items.push(block(api, seq++, {
			content: arg, block_kind: 'tool_call', tool_name: tool, summary: arg, agent: `sub-${i}`, model: 'haiku',
			parent_block_id: call.id, input_command_id: run, created_at: started
		}));
	});
	items.push(block(api, seq++, {
		content: 'sleep 600', block_kind: 'tool_call', tool_name: 'Bash', summary: 'Bash: sleep 600', agent: 'orchestrator',
		state: 'streaming', input_command_id: run, created_at: at(40_000), started_at: at(40_000)
	}));
	api.histories.set(id, items);
	api.bindConversation(id);
	const detail = api.details.get(id);
	if (detail) detail.activity = { state: 'working', evidence: { phase: 'running_tool' }, since: at(60_000) };
	await installChatRoutes(page, hub, api, OWNER);
	// Запись scrollTop из кода приложения: так видно, что лента не двигалась сама.
	await page.addInitScript(() => {
		const writes: number[] = [];
		(window as unknown as { __writes: number[] }).__writes = writes;
		const desc = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop')!;
		Object.defineProperty(Element.prototype, 'scrollTop', {
			configurable: true,
			get: desc.get,
			set(value: number) {
				if ((this as HTMLElement).classList?.contains('feed')) writes.push(value);
				desc.set!.call(this, value);
			}
		});
	});
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('.sub').first()).toBeVisible({ timeout: 15_000 });
	return { hub, api, id, calls, run };
}

type Live = Awaited<ReturnType<typeof boot>>;

/** Субагент берётся за другой инструмент. */
function nextPhase({ hub, api, id, calls, run }: Live, index: number, step: number): void {
	const [tool, arg] = PHASES[(index + step) % PHASES.length]!;
	const blockId = api.id();
	api.blocks.set(blockId, { text: arg, rev: 1, state: 'final' });
	hub.append(`conversation:${id}`, {
		kind: 'block_started', conversation_id: id,
		payload: { block_id: blockId, rev: 0, kind: 'tool_call', input_command_id: run, parent_block_id: calls[index]!.id, agent: `sub-${index}`, model: 'haiku', tool_name: tool, summary: arg, approved_by: null }
	});
	hub.flush();
}

/** Новый шаг самого ведущего: карточка хода растёт с хвоста. */
async function leadStep({ hub, api, id, run }: Live, n: number): Promise<void> {
	const blockId = api.id();
	const text = `src/file-${n}.rs`;
	api.blocks.set(blockId, { text, rev: 1, state: 'final' });
	const scope = `conversation:${id}`;
	hub.append(scope, { kind: 'block_started', conversation_id: id, payload: { block_id: blockId, rev: 0, kind: 'tool_call', input_command_id: run, parent_block_id: null, agent: 'orchestrator', model: 'opus-5.5', tool_name: 'Read', summary: text, approved_by: null } });
	hub.append(scope, { kind: 'block_final', conversation_id: id, payload: { block_id: blockId, rev: 1, text, checksum: await sha256Hex(text), text_omitted: false, summary: text } });
	hub.flush();
}

interface Sample {
	rows: number[];
	pills: number[];
	times: string[];
	texts: number[];
	statuses: string[];
	live: number;
	scrollHeight: number;
	scrollTop: number;
}

function sample(page: Page): Promise<Sample> {
	return page.evaluate(() => {
		const q = (sel: string): HTMLElement[] => [...document.querySelectorAll<HTMLElement>(sel)];
		const feed = document.querySelector('.feed') as HTMLElement;
		return {
			rows: q('.feed [data-row]').map((el) => el.getBoundingClientRect().height),
			pills: q('.sub .pill').map((el) => el.getBoundingClientRect().width),
			times: q('.sub .pill').map((el) => el.textContent ?? ''),
			texts: q('.sub .text').map((el) => el.getBoundingClientRect().width),
			statuses: q('.sub .status').map((el) => el.textContent ?? ''),
			live: document.querySelector('.live')?.getBoundingClientRect().height ?? 0,
			scrollHeight: feed.scrollHeight,
			scrollTop: feed.scrollTop
		};
	});
}

/** Метки ставятся на узлы до первого такта: уцелевший узел сохраняет метку, пересозданный её лишён. */
async function watch(page: Page): Promise<void> {
	await page.evaluate(() => {
		const feed = document.querySelector('.feed') as HTMLElement;
		for (const node of feed.querySelectorAll('*')) (node as unknown as { __mark: boolean }).__mark = true;
		const state = { shift: 0, supported: false, added: 0, removed: 0, text: 0, attrs: 0, writes: (window as unknown as { __writes: number[] }).__writes.length, stop: () => {} };
		let observer: PerformanceObserver | null = null;
		try {
			observer = new PerformanceObserver((list) => {
				for (const entry of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[]) if (!entry.hadRecentInput) state.shift += entry.value;
			});
			observer.observe({ type: 'layout-shift' });
			state.supported = true;
		} catch {
			observer = null;
		}
		const mutations = new MutationObserver((records) => {
			for (const record of records) {
				if (record.type === 'characterData') state.text += 1;
				else if (record.type === 'attributes') state.attrs += 1;
				else {
					state.added += record.addedNodes.length;
					state.removed += record.removedNodes.length;
				}
			}
		});
		mutations.observe(feed, { subtree: true, childList: true, characterData: true, attributes: true });
		state.stop = () => { observer?.disconnect(); mutations.disconnect(); };
		(window as unknown as { __watch: typeof state }).__watch = state;
	});
}

async function report(page: Page) {
	return page.evaluate(() => {
		const state = (window as unknown as { __watch: { shift: number; supported: boolean; added: number; removed: number; text: number; attrs: number; writes: number; stop: () => void } }).__watch;
		state.stop();
		const fresh = [...document.querySelectorAll('.feed *')].filter((el) => !(el as unknown as { __mark?: boolean }).__mark).length;
		const writes = (window as unknown as { __writes: number[] }).__writes.length - state.writes;
		return { shift: state.shift, supported: state.supported, added: state.added, removed: state.removed, text: state.text, attrs: state.attrs, recreated: fresh, writes };
	});
}

const spread = (values: number[]): number => (values.length === 0 ? 0 : Math.max(...values) - Math.min(...values));
const column = (samples: Sample[], key: 'rows' | 'pills' | 'texts'): number[][] => samples[0]![key].map((_, i) => samples.map((s) => s[key][i]!));

async function measure(page: Page, seconds: number, each?: (second: number) => Promise<void> | void): Promise<Sample[]> {
	await watch(page);
	const samples = [await sample(page)];
	for (let step = 1; step <= seconds * 2; step++) {
		await page.waitForTimeout(500);
		if (step % 2 === 0) await each?.(step / 2);
		samples.push(await sample(page));
	}
	return samples;
}

async function place(page: Page, mode: 'pinned' | 'mid_feed'): Promise<void> {
	const feed = page.locator('.feed');
	// Середина ленты: живая карточка на экране, но до хвоста ещё далеко.
	if (mode === 'mid_feed') await feed.evaluate((el) => { el.scrollTop = el.scrollHeight - el.clientHeight - 260; });
	else await feed.evaluate((el) => { el.scrollTop = el.scrollHeight; });
	await page.waitForTimeout(900);
	expect(await page.locator('.sub').count()).toBeGreaterThanOrEqual(2);
}

for (const mode of ['pinned', 'mid_feed'] as const) {
	test(`feed_timers_tick_in_place_${mode}`, async ({ page }) => {
		test.setTimeout(60_000);
		await boot(page, STEADY);
		await place(page, mode);
		const samples = await measure(page, 6);
		const seen = await report(page);
		expect(new Set(samples.map((s) => s.times.join('|'))).size, 'таймеры тикают').toBeGreaterThan(4);
		expect(seen.text, 'меняются текстовые узлы').toBeGreaterThanOrEqual(12);
		expect(seen).toMatchObject({ added: 0, removed: 0, attrs: 0, recreated: 0, writes: 0 });
		if (seen.supported) expect(seen.shift).toBe(0);
		expect(spread(samples.map((s) => s.scrollTop))).toBe(0);
		expect(spread(samples.map((s) => s.scrollHeight))).toBe(0);
		for (const key of ['rows', 'pills', 'texts'] as const) for (const values of column(samples, key)) expect(spread(values), key).toBe(0);
		expect(spread(samples.map((s) => s.live))).toBe(0);
	});

	test(`feed_subagent_phases_update_in_place_${mode}`, async ({ page }) => {
		test.setTimeout(60_000);
		const live = await boot(page, STEADY);
		await place(page, mode);
		const samples = await measure(page, 6, (second) => {
			if (second % 2 === 0) live.calls.forEach((_, index) => nextPhase(live, index, second / 2));
		});
		const seen = await report(page);
		expect(samples.at(-1)!.statuses, 'фазы сменились').not.toEqual(samples[0]!.statuses);
		expect(seen).toMatchObject({ added: 0, removed: 0, recreated: 0, writes: 0 });
		if (seen.supported) expect(seen.shift).toBe(0);
		expect(spread(samples.map((s) => s.scrollTop))).toBe(0);
		for (const key of ['rows', 'pills', 'texts'] as const) for (const values of column(samples, key)) expect(spread(values), key).toBe(0);
	});
}

test('feed_mid_position_holds_while_the_live_card_grows', async ({ page }) => {
	test.setTimeout(60_000);
	const live = await boot(page, STEADY);
	await place(page, 'mid_feed');
	const samples = await measure(page, 8, async (second) => {
		live.calls.forEach((_, index) => nextPhase(live, index, second));
		if (second % 2 === 0) await leadStep(live, second);
	});
	const seen = await report(page);
	expect(samples.at(-1)!.scrollHeight, 'карточка выросла').toBeGreaterThan(samples[0]!.scrollHeight);
	expect(spread(samples.map((s) => s.scrollTop)), 'лента не уехала').toBe(0);
	expect(seen.writes, 'приложение не двигало ленту').toBe(0);
	if (seen.supported) expect(seen.shift).toBeLessThan(0.01);
});

test('feed_time_pill_only_grows_across_digit_boundaries', async ({ page }) => {
	test.setTimeout(60_000);
	await boot(page, CROSSING);
	await place(page, 'pinned');
	const samples = await measure(page, 6);
	for (const widths of column(samples, 'pills')) {
		widths.forEach((width, i) => { if (i > 0) expect(width).toBeGreaterThanOrEqual(widths[i - 1]! - 0.01); });
		expect(new Set(widths.map((width) => width.toFixed(1))).size, 'ширина меняется не чаще раза на разряд').toBeLessThanOrEqual(3);
	}
	for (const heights of column(samples, 'rows')) expect(spread(heights)).toBe(0);
});
