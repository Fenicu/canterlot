import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const MINUTE = 60_000;

/** Старый субагент из истории: он стоит в ленте и в дереве сразу, без движения. */
function oldCall(api: FakeHubApi, run: string): HistoryBlock {
	const id = api.id();
	const at = new Date(Date.now() - 30 * MINUTE).toISOString();
	api.blocks.set(id, { text: 'старый', rev: 1, state: 'final' });
	return {
		kind: 'block', id, block_kind: 'tool_call', content: 'старый', content_bytes: 12, content_truncated: false, created_seq: '1', created_at: at, rev: 1,
		state: 'final', agent: 'claude', model: 'opus-5.5', tool_name: 'Agent', summary: 'старый: разобрать модуль', parent_block_id: null, input_command_id: run, line_count: 1,
		started_at: at, finished_at: at
	};
}

async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'Оркестратор', model: 'opus-5.5' });
	const run = api.id();
	api.histories.set(id, [oldCall(api, run)]);
	api.bindConversation(id);
	api.details.get(id)!.activity = { state: 'working', evidence: {}, since: new Date().toISOString() };
	await installChatRoutes(page, hub, api);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	return { hub, api, id, run };
}

/** Ведущий запускает субагента на глазах владельца. */
function spawnSubagent(hub: FakeHub, api: FakeHubApi, id: string, run: string, name: string, blockId = api.id()): string {
	api.blocks.set(blockId, { text: name, rev: 1, state: 'streaming' });
	hub.append(`conversation:${id}`, {
		kind: 'block_started', conversation_id: id,
		payload: { block_id: blockId, rev: 0, kind: 'tool_call', input_command_id: run, parent_block_id: null, agent: 'claude', model: 'opus-5.5', tool_name: 'Agent', summary: `${name}: проверить миграцию`, approved_by: null }
	});
	hub.flush();
	return blockId;
}

const names = (locator: import('@playwright/test').Locator) => locator.evaluate((node) => node.getAnimations().map((animation) => (animation as CSSAnimation).animationName ?? ''));

/**
 * Застаёт анимацию узла, пока она ещё идёт: ожидание и пауза выполняются в странице,
 * в первом же кадре после действия. Иначе загруженная машина успевает доиграть
 * короткий переход между вызовами Playwright.
 */
async function pauseMotion(page: Page, selector: string, act: () => unknown): Promise<string | null> {
	const caught = page.evaluate((selector) => new Promise<string | null>((resolve) => {
		const started = performance.now();
		const look = () => {
			const node = document.querySelector<HTMLElement>(selector);
			const [animation] = (node?.getAnimations() ?? []).filter((item) => Number(item.effect?.getComputedTiming().duration) > 0);
			if (node && animation) {
				animation.pause();
				(window as unknown as { __motion: HTMLElement }).__motion = node;
				resolve((animation as CSSAnimation).animationName ?? '');
			} else if (performance.now() - started > 8_000) resolve(null);
			else requestAnimationFrame(look);
		};
		look();
	}), selector);
	await act();
	return caught;
}

for (const motion of ['full', 'reduced'] as const) {
	test(`new_subagent_grows_out_of_its_branch_in_the_feed_${motion}`, async ({ page }) => {
		if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
		const { hub, api, id, run } = await boot(page);
		const old = page.locator('.feed .sub').first();
		await expect(old).toBeVisible();
		// История стоит сразу.
		await expect(old).not.toHaveAttribute('data-grows', '');

		let name: string | null = null;
		if (motion === 'full') name = await pauseMotion(page, '.feed .sub[data-grows]', () => { spawnSubagent(hub, api, id, run, 'новый'); });
		else spawnSubagent(hub, api, id, run, 'новый');
		const fresh = page.locator('.feed .sub').filter({ hasText: 'новый' });
		await expect(fresh).toHaveAttribute('data-grows', '');
		const heightBefore = (await fresh.boundingBox())!.height;
		if (motion === 'reduced') {
			expect(name).toBeNull();
			expect(await names(fresh)).toEqual([]);
		} else {
			expect(name?.endsWith('branch-reveal')).toBe(true);
			// Анимация стоит на паузе: срезы не зависят от того, сколько кадров успел нарисовать браузер.
			const growth = await page.evaluate(async () => {
				const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
				const node = (window as unknown as { __motion: HTMLElement }).__motion;
				const [animation] = node.getAnimations().filter((item) => Number(item.effect?.getComputedTiming().duration) > 0);
				const total = Number(animation!.effect!.getComputedTiming().duration);
				const width = node.getBoundingClientRect().width;
				// Ширину видимой части меряем попаданием в строку: clip-path обрезает и отрисовку, и попадание,
				// а вычисленный clip в Firefox может остаться неразобранным calc().
				const visibleWidth = (): number => {
					const box = node.getBoundingClientRect();
					const y = Math.min(Math.max(box.top + box.height / 2, 0), window.innerHeight - 1);
					let low = 0;
					let high = Math.max(box.width, 0);
					for (let step = 0; step < 12; step++) {
						const middle = (low + high) / 2;
						const hit = document.elementFromPoint(box.left + middle, y);
						if (hit && node.contains(hit)) low = middle;
						else high = middle;
					}
					return low;
				};
				const visible: number[] = [];
				const heights: number[] = [];
				for (const share of [0, 0.2, 0.4, 0.6, 1]) {
					animation!.currentTime = total * share;
					await frame();
					await frame();
					visible.push(visibleWidth());
					heights.push(node.getBoundingClientRect().height);
				}
				animation!.finish();
				return { visible, heights, width };
			});
			// Строка открывается от значка ветки: сперва видна только левая часть, дальше — всё шире и до конца.
			expect(growth.visible[0]!).toBeGreaterThan(0);
			expect(growth.visible[0]!).toBeLessThan(growth.width * 0.5);
			for (let index = 1; index < growth.visible.length; index++) {
				expect(growth.visible[index]!).toBeGreaterThan(growth.visible[index - 1]!);
			}
			expect(growth.visible[growth.visible.length - 1]!).toBeGreaterThanOrEqual(growth.width - 1);
			// Высота строки во время появления не менялась: ленте нечего перемерять.
			for (const height of growth.heights) expect(Math.abs(height - heightBefore)).toBeLessThanOrEqual(1);
		}
		await expect.poll(() => fresh.evaluate((node) => node.getAnimations().length), { timeout: 3_000 }).toBe(0);
		expect((await fresh.boundingBox())!.height).toBe(heightBefore);
	});
}

test('new_subagent_grows_in_the_delegation_tree_while_the_opened_tree_stands', async ({ page, isMobile }) => {
	const { hub, api, id, run } = await boot(page);
	if (!isMobile) await page.setViewportSize({ width: 1440, height: 900 });
	if (isMobile) await page.getByRole('button', { name: /^Делегирование/ }).click();
	else await page.locator('.topbar').getByRole('button', { name: 'Делегирование' }).click();
	const scope = isMobile ? page.getByRole('dialog', { name: 'Делегирование' }) : page.locator('[data-pane="lane"]');
	await expect(scope.locator('.tree')).toBeVisible();
	const moving = () => scope.locator('[data-node]').evaluateAll((nodes) => nodes.filter((node) => node.getAnimations().length > 0).map((node) => (node as HTMLElement).dataset.node));
	// Открытое дерево не вырастает целиком: узлы, бывшие при открытии, стоят.
	await page.waitForTimeout(300);
	expect(await moving()).toEqual([]);

	const blockId = api.id();
	const caught = await pauseMotion(page, `[data-node="${blockId}"]`, () => { spawnSubagent(hub, api, id, run, 'новый', blockId); });
	expect(caught).not.toBeNull();
	// Новый узел — единственный, кто движется; пойманная анимация ещё стоит на паузе.
	expect(await moving()).toEqual([blockId]);
	await page.evaluate(() => {
		for (const animation of (window as unknown as { __motion: HTMLElement }).__motion.getAnimations()) animation.finish();
	});
	await expect(scope.locator(`[data-node="${blockId}"]`)).toBeVisible();
	await expect.poll(moving, { timeout: 3_000 }).toEqual([]);
});
