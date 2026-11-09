import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const TURNS = 60;
const at = (msAgo: number): string => new Date(Date.now() - msAgo).toISOString();

function block(api: FakeHubApi, seq: number, run: string, patch: Partial<HistoryBlock> & { content: string }): HistoryBlock {
	const id = api.id();
	api.blocks.set(id, { text: patch.content, rev: 1, state: 'final' });
	return {
		kind: 'block', id, block_kind: 'text', content_bytes: patch.content.length, content_truncated: false,
		created_seq: String(seq), created_at: at((TURNS - seq / 3) * 60_000), rev: 1, state: 'final', agent: 'claude', model: 'opus-5.5',
		tool_name: null, summary: null, parent_block_id: null, input_command_id: run, line_count: 1, ...patch
	};
}

/** Длинная лента: в каждом ходе текст и один вызов инструмента с многострочным выводом. */
async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'длинная лента', model: 'opus-5.5' });
	const items: HistoryBlock[] = [];
	let seq = 1;
	for (let turn = 0; turn < TURNS; turn++) {
		const run = api.id();
		items.push(block(api, seq++, run, { content: `ход ${turn}: ${'проверяю модуль и сверяю результат. '.repeat(2 + (turn % 3))}` }));
		const body = Array.from({ length: 12 }, (_, line) => `строка ${line} файла ${turn}`).join('\n');
		items.push(block(api, seq++, run, { content: body, block_kind: 'tool_call', tool_name: 'Read', summary: `src/file-${turn}.rs`, line_count: 12 }));
		items.push(block(api, seq++, run, { content: `итог хода ${turn}` }));
	}
	api.histories.set(id, items);
	api.bindConversation(id);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.locator('.feed .tool').first()).toBeVisible({ timeout: 15_000 });
}

/** Уводит ленту от хвоста и выбирает шапку блока посреди экрана. */
async function pickMiddleHead(page: Page): Promise<string> {
	const feed = page.locator('.feed');
	await feed.evaluate((el) => el.scrollBy(0, -el.clientHeight * 3));
	await expect(feed).not.toHaveAttribute('data-pinned', '');
	await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
	// Окно ленты перерисовывается после прокрутки не в фиксированный кадр: ищем, пока шапка не встанет в середину.
	let key = '';
	await expect
		.poll(async () => {
			key = await feed.evaluate((el) => {
				const box = el.getBoundingClientRect();
				const heads = [...el.querySelectorAll<HTMLElement>('.tool > .head')];
				const head = heads.find((node) => {
					const top = node.getBoundingClientRect().top;
					return top > box.top + box.height * 0.25 && top < box.top + box.height * 0.5;
				});
				return head?.closest<HTMLElement>('[data-row]')?.dataset.key ?? '';
			});
			return key;
		})
		.not.toBe('');
	return key;
}

/**
 * Срезы движения: переход ищется и ставится на паузу одним кадром вместе с действием, а не отдельным
 * запросом после него. Иначе между ожиданием и снятием кадров проходит лишний круг, и под нагрузкой
 * переход успевает кончиться — срез остаётся пустым.
 */
async function slices(page: Page, key: string, act: () => Promise<unknown>, budgetMs: number): Promise<{ head: number[]; fold: number[] } | null> {
	const row = `[data-row][data-key="${key}"]`;
	const waiting = page.evaluate(({ row, budgetMs }) => new Promise<boolean>((resolve) => {
		const started = performance.now();
		const look = () => {
			const fold = document.querySelector<HTMLElement>(`${row} .fold`);
			const [animation] = (fold?.getAnimations() ?? []).filter((item) => Number(item.effect?.getComputedTiming().duration) > 0);
			if (fold && animation) {
				animation.pause();
				(window as unknown as { __fold: [HTMLElement, Animation] }).__fold = [fold, animation];
				resolve(true);
			} else if (performance.now() - started > budgetMs) resolve(false);
			else requestAnimationFrame(look);
		};
		look();
	}), { row, budgetMs });
	await act();
	if (!(await waiting)) return null;
	return page.evaluate(async (row) => {
		const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
		const [fold, animation] = (window as unknown as { __fold: [HTMLElement, Animation] }).__fold;
		const total = Number(animation.effect!.getComputedTiming().duration);
		const out = { head: [] as number[], fold: [] as number[] };
		for (const share of [0.1, 0.3, 0.6]) {
			animation.currentTime = total * share;
			await frame();
			await frame();
			out.head.push(document.querySelector(`${row} .head`)!.getBoundingClientRect().top);
			out.fold.push(fold.getBoundingClientRect().height);
		}
		animation.play();
		// Переход доигрывает до конца: следующее действие не должно попасть в его середину.
		await Promise.race([animation.finished.catch(() => {}), new Promise((resolve) => setTimeout(resolve, 3_000))]);
		return out;
	}, row);
}

const headTop = (page: Page, key: string) => page.locator(`[data-row][data-key="${key}"] .head`).evaluate((el) => el.getBoundingClientRect().top);

for (const motion of ['full', 'reduced'] as const) {
	test(`tool_block_unfolds_and_folds_smoothly_without_moving_the_feed_${motion}`, async ({ page }) => {
		test.setTimeout(60_000);
		if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
		await boot(page);
		const key = await pickMiddleHead(page);
		const head = page.locator(`[data-row][data-key="${key}"] .head`);
		const top = await headTop(page, key);

		const opening = await slices(page, key, () => head.click(), motion === 'reduced' ? 1_500 : 8_000);
		if (motion === 'reduced') expect(opening).toBeNull();
		else {
			expect(opening).not.toBeNull();
			// Тело растёт, а шапка, которую тронули, стоит на месте: лента не прыгает.
			expect(opening!.fold[0]!).toBeLessThan(opening!.fold[1]!);
			expect(opening!.fold[1]!).toBeLessThan(opening!.fold[2]!);
			for (const y of opening!.head) expect(Math.abs(y - top)).toBeLessThanOrEqual(1.5);
		}
		await expect(page.locator(`[data-row][data-key="${key}"] .fold`)).toBeVisible();
		await expect.poll(() => page.locator(`[data-row][data-key="${key}"] .fold`).evaluate((el) => el.getAnimations().length)).toBe(0);
		expect(Math.abs((await headTop(page, key)) - top)).toBeLessThanOrEqual(1.5);
		// Виртуализатор знает итоговую высоту: соседние строки стоят вплотную, без зазора и наезда.
		const gap = await page.evaluate((key) => {
			const row = document.querySelector<HTMLElement>(`[data-row][data-key="${key}"]`)!;
			const next = row.nextElementSibling as HTMLElement;
			return next.getBoundingClientRect().top - row.getBoundingClientRect().bottom;
		}, key);
		expect(Math.abs(gap)).toBeLessThanOrEqual(1);

		const closing = await slices(page, key, () => head.click(), motion === 'reduced' ? 1_500 : 8_000);
		if (motion === 'reduced') expect(closing).toBeNull();
		else {
			expect(closing).not.toBeNull();
			expect(closing!.fold[0]!).toBeGreaterThan(closing!.fold[1]!);
			expect(closing!.fold[1]!).toBeGreaterThan(closing!.fold[2]!);
			for (const y of closing!.head) expect(Math.abs(y - top)).toBeLessThanOrEqual(1.5);
		}
		await expect(page.locator(`[data-row][data-key="${key}"] .fold`)).toHaveCount(0);
		expect(Math.abs((await headTop(page, key)) - top)).toBeLessThanOrEqual(1.5);
	});
}

test('open_block_returning_to_the_window_does_not_unfold_again', async ({ page }) => {
	test.setTimeout(60_000);
	await boot(page);
	const key = await pickMiddleHead(page);
	await page.locator(`[data-row][data-key="${key}"] .head`).click();
	await expect.poll(() => page.locator(`[data-row][data-key="${key}"] .fold`).evaluate((el) => el.getAnimations().length)).toBe(0);
	const feed = page.locator('.feed');
	// Строка уходит из окна ленты и возвращается: окно монтирует её заново уже раскрытой.
	const away = await feed.evaluate((el) => el.clientHeight * 4);
	await feed.evaluate((el, away) => el.scrollBy(0, -away), away);
	await expect(page.locator(`[data-row][data-key="${key}"]`)).toHaveCount(0);
	// Подгрузка старых страниц сдвигает позицию: назад идём шагами, пока строка не вернётся в окно.
	const row = page.locator(`[data-row][data-key="${key}"]`);
	await expect
		.poll(async () => {
			if ((await row.count()) > 0) return true;
			await feed.evaluate((el) => el.scrollBy(0, el.clientHeight / 2));
			return false;
		}, { timeout: 30_000, intervals: [100] })
		.toBe(true);
	await row.waitFor();
	const fold = page.locator(`[data-row][data-key="${key}"] .fold`);
	await expect(fold).toBeVisible();
	expect(await fold.evaluate((el) => el.getAnimations().length)).toBe(0);
});
