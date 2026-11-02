import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

test.use({ viewport: { width: 384, height: 800 } });

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, seed?: (api: FakeHubApi, id: string) => void) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api);
	seed?.(api, id);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	return { hub, api, id };
}

function liveExecution(api: FakeHubApi, id: string): void {
	const detail = api.details.get(id) as Record<string, unknown>;
	detail.execution = { state: 'running', generation: 1 };
}

async function indicator(page: Page) {
	return page.getByRole('button', { name: /^Контекст/ });
}

for (const theme of ['dark', 'light'] as const) {
	test(`compaction_running_line_finished_lines_and_indicator_${theme}`, async ({ page }) => {
		const { hub, api, id } = await boot(page, (seed, conversation) => {
			seed.pushContext(conversation, 640_000, 1_000_000);
			seed.pushCompaction(conversation, { trigger: 'auto', ok: true, pre_tokens: 980_000 });
			seed.pushCompaction(conversation, { trigger: 'manual', ok: true });
			seed.pushCompaction(conversation, { trigger: 'auto', ok: false });
			liveExecution(seed, conversation);
		});
		await page.evaluate((value) => localStorage.setItem('canterlot.theme', value), theme);
		await page.reload();
		await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

		const feed = page.locator('.feed');
		await expect(feed.getByText('Контекст сжат автоматически, было 980 тыс. токенов')).toBeVisible();
		await expect(feed.getByText('Контекст сжат вручную')).toBeVisible();
		await expect(feed.getByText('Сжатие контекста не удалось')).toBeVisible();

		hub.append(`conversation:${id}`, {
			kind: 'phase_changed',
			conversation_id: id,
			payload: { phase: 'context_maintenance', tool_name: null, summary: null }
		});
		hub.flush();
		const running = feed.getByText('Сжимает контекст');
		await expect(running).toBeVisible();
		const pulse = page.locator('.compact[data-running="true"] .pulse');
		await expect(pulse).toBeVisible();
		const animated = await pulse.evaluate((el) => getComputedStyle(el).animationName);
		if (await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches)) {
			expect(animated).toBe('none');
		} else {
			expect(animated).not.toBe('none');
		}

		const ctx = await indicator(page);
		await expect(ctx).toBeVisible();
		await expect(ctx).toContainText('Контекст 64%');
		await expect(ctx).toHaveAttribute('data-tone', 'accent');
		api.pushContext(id, 750_000, 1_000_000);
		hub.flush();
		await expect(ctx).toContainText('Контекст 75%');
		await expect(ctx).toHaveAttribute('data-tone', 'warning');
		api.pushContext(id, 950_000, 1_000_000);
		hub.flush();
		await expect(ctx).toContainText('Контекст 95%');
		await expect(ctx).toHaveAttribute('data-tone', 'danger');

		await ctx.click();
		const details = page.locator('[data-details]');
		await expect(details).toBeVisible();
		await expect(details).toContainText('950 тыс. из 1 млн токенов');

		let posts = 0;
		await page.route('**/api/v1/conversations/*/compact', async (route) => {
			if (route.request().method() !== 'POST') return route.fallback();
			posts += 1;
			await route.fulfill({
				status: 200,
				headers: { 'content-type': 'application/json' },
				json: { requested: { command_id: '00000000-0000-4000-8000-000000000099' } }
			});
		});
		const squeeze = details.getByRole('button', { name: 'Сжать контекст' });
		await expect(squeeze).toBeEnabled();
		await squeeze.click();
		await expect(details.getByRole('button', { name: 'Точно сжать?' })).toBeVisible();
		expect(posts).toBe(0);
		await details.getByRole('button', { name: 'Точно сжать?' }).click();
		await expect.poll(() => posts).toBe(1);
		await expect(details).toBeHidden();

		const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
		expect(overflow).toBeLessThanOrEqual(0);
	});

	test(`compaction_indicator_disabled_states_${theme}`, async ({ page }) => {
		const { api, id } = await boot(page, (seed, conversation) => {
			seed.pushContext(conversation, 100_000, 1_000_000);
			const detail = seed.details.get(conversation) as Record<string, unknown>;
			detail.activity = { state: 'working', evidence: { phase: 'generating' }, since: '2026-10-28T12:00:00.000Z' };
			(detail.queue as Record<string, unknown>).run_slot = { running: { input_id: '00000000-0000-4000-8000-000000000010' } };
			detail.execution = { state: 'running', generation: 1 };
		});
		await page.evaluate((value) => localStorage.setItem('canterlot.theme', value), theme);
		await page.reload();
		await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

		const ctx = await indicator(page);
		await ctx.click();
		const details = page.locator('[data-details]');
		await expect(details).toContainText('Сжатие доступно между ходами');
		await expect(details.getByRole('button', { name: 'Сжать контекст' })).toBeDisabled();
		void api;
		void id;
	});

	test(`compaction_indicator_no_live_execution_caption_${theme}`, async ({ page }) => {
		await boot(page, (seed, conversation) => {
			seed.pushContext(conversation, 100_000, 1_000_000);
		});
		await page.evaluate((value) => localStorage.setItem('canterlot.theme', value), theme);
		await page.reload();
		await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });

		const ctx = await indicator(page);
		await ctx.click();
		const details = page.locator('[data-details]');
		await expect(details).toContainText('Нет живого исполнения для сжатия');
		await expect(details.getByRole('button', { name: 'Сжать контекст' })).toBeDisabled();
	});
}
