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
function spawnSubagent(hub: FakeHub, api: FakeHubApi, id: string, run: string, name: string): string {
	const blockId = api.id();
	api.blocks.set(blockId, { text: name, rev: 1, state: 'streaming' });
	hub.append(`conversation:${id}`, {
		kind: 'block_started', conversation_id: id,
		payload: { block_id: blockId, rev: 0, kind: 'tool_call', input_command_id: run, parent_block_id: null, agent: 'claude', model: 'opus-5.5', tool_name: 'Agent', summary: `${name}: проверить миграцию`, approved_by: null }
	});
	hub.flush();
	return blockId;
}

const names = (locator: import('@playwright/test').Locator) => locator.evaluate((node) => node.getAnimations().map((animation) => (animation as CSSAnimation).animationName ?? ''));

for (const motion of ['full', 'reduced'] as const) {
	test(`new_subagent_grows_out_of_its_branch_in_the_feed_${motion}`, async ({ page }) => {
		if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
		const { hub, api, id, run } = await boot(page);
		const old = page.locator('.feed .sub').first();
		await expect(old).toBeVisible();
		// История стоит сразу.
		await expect(old).not.toHaveAttribute('data-grows', '');

		spawnSubagent(hub, api, id, run, 'новый');
		const fresh = page.locator('.feed .sub').filter({ hasText: 'новый' });
		await expect(fresh).toHaveAttribute('data-grows', '');
		const heightBefore = (await fresh.boundingBox())!.height;
		if (motion === 'reduced') expect(await names(fresh)).toEqual([]);
		else {
			expect((await names(fresh)).some((name) => name.endsWith('branch-reveal'))).toBe(true);
			// Строка открывается от значка ветки: в начале видна только левая часть.
			const clip = await fresh.evaluate((node) => {
				const [animation] = node.getAnimations();
				animation!.pause();
				animation!.currentTime = 0;
				const value = getComputedStyle(node).clipPath;
				animation!.play();
				return value;
			});
			expect(clip).toMatch(/^inset\(/);
			expect(clip).not.toMatch(/^inset\(0px( 0px)*( round|\))/);
		}
		await expect.poll(() => fresh.evaluate((node) => node.getAnimations().length), { timeout: 3_000 }).toBe(0);
		// Высота строки во время появления не менялась: ленте нечего перемерять.
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

	const blockId = spawnSubagent(hub, api, id, run, 'новый');
	await expect(scope.locator(`[data-node="${blockId}"]`)).toBeVisible();
	await expect.poll(moving, { timeout: 1_500 }).toEqual([blockId]);
	await expect.poll(moving, { timeout: 3_000 }).toEqual([]);
});
