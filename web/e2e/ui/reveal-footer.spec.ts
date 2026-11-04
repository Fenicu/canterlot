import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { sha256Hex } from '../../src/lib/stores/blocks';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';

type Live = { hub: FakeHub; api: FakeHubApi; id: string; run: string };

async function setMotion(page: Page, motion: 'normal' | 'off'): Promise<void> {
	await page.addInitScript((value) => {
		try { localStorage.setItem('canterlot.motion', value); } catch { /* настройка действует и без хранилища */ }
	}, motion);
}

/** Чат с живым ходом: статус идёт, а подвал карточки виден во время потока. */
async function boot(page: Page): Promise<Live> {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api);
	const detail = api.details.get(id);
	if (detail) {
		detail.activity = { state: 'working', evidence: { phase: 'generating' }, since: AT };
		detail.last_activity_at = AT;
	}
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	const run = api.id();
	hub.append(`conversation:${id}`, {
		kind: 'derived_changed',
		conversation_id: id,
		payload: {
			conversation_id: id,
			activity: { state: 'working', evidence: { phase: 'generating' }, since: AT },
			unread_count: 0,
			queue: { queued: 0, pause_reasons: [], hold_reasons: [] },
			last_activity_at: AT,
			settings: { model: 'opus', effort: 'medium', permission_mode: 'owner' },
			crunch: { enabled: false }
		}
	});
	hub.flush();
	return { hub, api, id, run };
}

function started(blockId: string, run: string) {
	return {
		block_id: blockId, rev: 0, kind: 'text', input_command_id: run, parent_block_id: null,
		agent: 'claude', model: 'opus', tool_name: null, summary: null, approved_by: null
	};
}

const PLAIN = Array.from({ length: 22 }, (_, i) => `строка ${i} несёт довольно много слов чтобы перенос был не один раз`).join('\n');
const MARKDOWN = [
	...Array.from({ length: 16 }, (_, i) => `- пункт ${i} с несколькими словами для переноса строки в списке`),
	'',
	'```ts',
	...Array.from({ length: 12 }, (_, i) => `const value${i} = "строка кода номер ${i}";`),
	'```'
].join('\n');

interface Sample {
	t: number;
	foot: number | null;
	block: number | null;
}

async function startWatch(page: Page, blockId: string): Promise<void> {
	await page.evaluate((id) => {
		const samples: Sample[] = [];
		(window as unknown as { __footerSamples: Sample[]; __footerStop: boolean }).__footerSamples = samples;
		(window as unknown as { __footerStop: boolean }).__footerStop = false;
		const tick = () => {
			const state = window as unknown as { __footerStop: boolean; __footerSamples: Sample[] };
			if (state.__footerStop) return;
			const foot = [...document.querySelectorAll<HTMLElement>('[data-role="answer-footer"]')].at(-1);
			const block = document.querySelector<HTMLElement>(`[data-block="${id}"]`);
			state.__footerSamples.push({
				t: performance.now(),
				foot: foot?.getBoundingClientRect().top ?? null,
				block: block?.getBoundingClientRect().height ?? null
			});
			requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	}, blockId);
}

async function stopWatch(page: Page): Promise<Sample[]> {
	return page.evaluate(() => {
		(window as unknown as { __footerStop: boolean }).__footerStop = true;
		return (window as unknown as { __footerSamples: Sample[] }).__footerSamples;
	});
}

function maxUp(samples: Sample[], key: 'foot' | 'block'): number {
	let worst = 0;
	for (let i = 1; i < samples.length; i++) {
		const before = samples[i - 1]![key];
		const after = samples[i]![key];
		if (before !== null && after !== null) worst = Math.max(worst, before - after);
	}
	return Math.round(worst * 10) / 10;
}

async function stream(page: Page, live: Live, text: string): Promise<string> {
	const blockId = live.api.id();
	const scope = `conversation:${live.id}`;
	live.hub.append(scope, { kind: 'block_started', conversation_id: live.id, payload: started(blockId, live.run) });
	live.hub.flush();
	await expect(page.locator('[data-role="answer-footer"]').last()).toBeVisible();
	await startWatch(page, blockId);
	let rev = 0;
	for (let i = 0; i < text.length; ) {
		const append = text.slice(i, i + 28);
		i += append.length;
		rev += 1;
		live.hub.append(scope, { kind: 'block_delta', conversation_id: live.id, payload: { block_id: blockId, base_rev: rev - 1, rev, append } });
		live.hub.flush();
		await page.waitForTimeout(24);
	}
	return blockId;
}

/** Ждёт, пока прокрутка перестанет догонять разметку: иначе замер до перехода ловит промежуточный кадр. */
async function waitForScrollStill(page: Page): Promise<void> {
	await page.evaluate(() => new Promise<void>((resolve) => {
		const feed = document.querySelector<HTMLElement>('.feed')!;
		let last = Number.NaN;
		let still = 0;
		let frames = 0;
		const tick = () => {
			const top = feed.scrollTop;
			if (Math.abs(top - last) < 0.01) still += 1;
			else { still = 0; last = top; }
			frames += 1;
			if (still >= 10 || frames >= 240) resolve();
			else requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	}));
}

async function finish(page: Page, live: Live, blockId: string, text: string): Promise<void> {
	const scope = `conversation:${live.id}`;
	// Подвал стоит на месте, пока лента прижата к низу: для проверки высоты готовой
	// разметки отрываемся от низа. Уменьшаем окно, чтобы даже короткий ответ прокручивался.
	const viewport = page.viewportSize();
	if (viewport && viewport.height > 600) await page.setViewportSize({ width: viewport.width, height: 600 });
	await page.waitForTimeout(100);
	const feed = page.locator('.feed');
	await feed.evaluate((el) => { el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight - 200); });
	await expect(feed).not.toHaveAttribute('data-pinned', '');
	await waitForScrollStill(page);
	const before = await page.locator('[data-role="answer-footer"]').last().evaluate((node) => node.getBoundingClientRect().top);
	live.hub.append(scope, {
		kind: 'block_final', conversation_id: live.id,
		payload: { block_id: blockId, rev: 999, text, checksum: await sha256Hex(text), text_omitted: false, summary: null }
	});
	live.hub.flush();
	await expect(page.locator(`[data-block="${blockId}"] .tail`)).toHaveCount(0);
	await waitForScrollStill(page);
	const after = await page.locator('[data-role="answer-footer"]').last().evaluate((node) => node.getBoundingClientRect().top);
	expect(after, `готовый ответ уехал вверх на ${Math.round((before - after) * 10) / 10} px`).toBeGreaterThanOrEqual(before - 1);
}

for (const [name, text] of [['plain', PLAIN], ['markdown', MARKDOWN]] as const) {
	test(`answer_footer_only_moves_down_while_streaming_${name}`, async ({ page }) => {
		test.setTimeout(60_000);
		await setMotion(page, 'normal');
		const live = await boot(page);
		const blockId = await stream(page, live, text);
		await page.waitForTimeout(300);
		const samples = (await stopWatch(page)).filter((sample) => sample.foot !== null);
		expect(samples.length, 'кадры потока попали в выборку').toBeGreaterThan(20);
		expect(maxUp(samples, 'block'), 'высота ответа уменьшилась').toBeLessThanOrEqual(1);
		expect(maxUp(samples, 'foot'), 'подвал уехал вверх во время потока').toBeLessThanOrEqual(1);
		await finish(page, live, blockId, text);
	});
}
