import type { Page } from '@playwright/test';
import type { HistoryBlock } from '../../src/lib/stores/chat.svelte';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const at = (msAgo: number): string => new Date(Date.now() - msAgo).toISOString();

async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title: 'работа', model: 'opus-5.5' });
	const blockId = api.id();
	api.blocks.set(blockId, { text: 'Разбираю модуль', rev: 1, state: 'streaming' });
	const item: HistoryBlock = {
		kind: 'block', id: blockId, block_kind: 'text', content: 'Разбираю модуль', content_bytes: 15, content_truncated: false,
		created_seq: '1', created_at: at(5_000), rev: 1, state: 'streaming', agent: 'claude', model: 'opus-5.5',
		tool_name: null, summary: null, parent_block_id: null, input_command_id: api.id(), line_count: 1
	};
	api.histories.set(id, [item]);
	api.bindConversation(id);
	api.details.get(id)!.activity = { state: 'working', evidence: {}, since: at(5_000) };
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	return { hub, api, id };
}

for (const motion of ['full', 'reduced'] as const) {
	test(`running_turn_shows_the_writing_line_not_the_compaction_fold_${motion}`, async ({ page }) => {
		if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
		await boot(page);
		const live = page.locator('.feed .live[data-working]');
		await expect(live).toBeVisible();
		// Мотив работы свой: строки сжатия в нём нет, а слова пишутся вдоль одной линии.
		await expect(live.locator('.fold')).toHaveCount(0);
		const words = live.locator('.write i');
		await expect(words).toHaveCount(3);
		const shape = await live.locator('.write').evaluate((node) => {
			const boxes = [...node.children].map((child) => child.getBoundingClientRect());
			return { tops: boxes.map((box) => Math.round(box.top)), lefts: boxes.map((box) => box.left), names: [...node.children].map((child) => getComputedStyle(child).animationName) };
		});
		expect(new Set(shape.tops).size).toBe(1);
		expect(shape.lefts[0]!).toBeLessThan(shape.lefts[1]!);
		expect(shape.lefts[1]!).toBeLessThan(shape.lefts[2]!);
		if (motion === 'reduced') {
			expect(shape.names).toEqual(['none', 'none', 'none']);
			// Без движения слова стоят написанными: строка всё равно говорит «работает».
			expect(await words.first().evaluate((node) => getComputedStyle(node).transform)).toBe('none');
		} else {
			expect(shape.names.every((name) => /work-write-(first|second|third)$/.test(name))).toBe(true);
		}
		// Мотив не меняет высоту строки ленты, пока движется.
		const heights = await page.evaluate(async () => {
			const row = document.querySelector('.feed .live')!.closest('[data-row]')!;
			const seen: number[] = [];
			for (let frame = 0; frame < 40; frame++) {
				seen.push(row.getBoundingClientRect().height);
				await new Promise((resolve) => requestAnimationFrame(resolve));
			}
			return seen;
		});
		expect(Math.max(...heights) - Math.min(...heights)).toBe(0);
	});
}
