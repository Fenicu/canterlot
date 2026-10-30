import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup05, seedMockup11 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

async function bootLane(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

	seedMachine(api, {
		id: 'workstation',
		name: 'workstation',
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [
					{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] },
					{ id: 'haiku', title: 'haiku', efforts: ['low'] }
				],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});

	const { id, rootBlockId } = seedMockup05(api);
	await installChatRoutes(page, hub, api);

	return { hub, api, id, rootBlockId };
}

test('lane_uses_the_same_renderer_as_the_main_chat', async ({ page }) => {
	const { id, rootBlockId } = await bootLane(page);

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();

	// Timeline structure: answer cards, details tool blocks
	await expect(page.locator('.lane-timeline-container .feed')).toBeVisible();
	await expect(page.locator('.card').first()).toBeVisible();
	await expect(page.locator('.tool').first()).toBeVisible();
});

test('lane_has_no_composer_and_explains_that_the_result_returns', async ({ page }) => {
	const { id, rootBlockId } = await bootLane(page);

	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible();

	// Verify no composer is rendered in lane
	await expect(page.locator('form.composer')).toHaveCount(0);
	await expect(page.locator('textarea[aria-label="Сообщение агенту"]')).toHaveCount(0);

	// Verify result returns message and link
	await expect(page.getByText('Результат вернётся в основную сессию')).toBeVisible();
	await expect(page.getByRole('link', { name: 'К основной сессии' })).toBeVisible();
});

test('desktop_delegation_panel_lists_nodes_with_state_labels_and_selects_a_lane', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

	seedMachine(api, {
		id: 'workstation',
		name: 'workstation',
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [
					{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] },
					{ id: 'haiku', title: 'haiku', efforts: ['low'] }
				],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});

	const { id } = seedMockup11(api);
	await installChatRoutes(page, hub, api);

	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-layout="desktop"]')).toBeVisible();

	// Open delegation panel
	await page.getByRole('button', { name: 'Делегирование' }).click();
	const panel = page.locator('[data-pane="lane"]');
	await expect(panel).toBeVisible();

	// Verify tree root and nodes
	await expect(panel.getByText('Восстановление потока')).toBeVisible();
	await expect(panel.getByText('Explore').first()).toBeVisible();
	await expect(panel.getByText('Читает')).toBeVisible();
	await expect(panel.getByText('Codex')).toBeVisible();
	await expect(panel.getByText('Ждёт')).toBeVisible();

	// Click node to open its lane in pane
	await panel.getByText('Explore').first().click();
	await expect(panel.locator('.selected-lane')).toBeVisible();
	await expect(panel.getByText('Результат вернётся в основную сессию')).toBeVisible();
});

test('desktop_pane_opens_and_closes_without_losing_chat_scroll', async ({ page }) => {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);

	seedMachine(api, {
		id: 'workstation',
		name: 'workstation',
		harnesses: [
			{
				id: 'claude',
				title: 'Claude Code',
				provider: 'claude',
				models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low'] }],
				permission_modes: ['owner']
			}
		]
	});

	const { id } = seedMockup11(api);
	await installChatRoutes(page, hub, api);

	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto(`/c/${id}`);

	const stream = page.locator('.feed');
	await expect(stream).toBeVisible();

	// Measure scroll before opening pane
	const initialScroll = await stream.evaluate((node) => node.scrollTop);

	// Open lane pane
	await page.getByRole('button', { name: 'Делегирование' }).click();
	const panel = page.locator('[data-pane="lane"]');
	await expect(panel).toBeVisible();

	// Close lane pane using close button
	await panel.getByRole('button', { name: 'Закрыть боковую панель' }).click();
	await expect(panel).toHaveCount(0);

	// Measure scroll after closing pane
	const finalScroll = await stream.evaluate((node) => node.scrollTop);
	expect(finalScroll).toBe(initialScroll);
});
