import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup05 } from '../../testing/fixtures/mockup-chat';
import { TAB_TITLE_LIMIT } from '../../src/lib/format/pageTitle';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const LONG_TITLE = 'Разобрать повторное применение событий и починить дедупликацию во всех очередях';

async function bootChat(page: Page, title = 'Сессия Альфа') {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const id = seedConversation(api, { title });
	await installChatRoutes(page, hub, api, OWNER);
	return { hub, api, id };
}

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
				models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] }],
				permission_modes: ['owner', 'ask', 'bypass', 'read_only']
			}
		]
	});
	const { id, rootBlockId } = seedMockup05(api);
	await installChatRoutes(page, hub, api, OWNER);
	return { hub, api, id, rootBlockId };
}

test('chat_tab_title_uses_the_session_title_and_follows_a_rename', async ({ page }) => {
	const { id } = await bootChat(page);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('heading', { level: 1 })).toHaveText('Сессия Альфа');
	await expect(page).toHaveTitle('Сессия Альфа · Canterlot');

	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Название, цвет и группы' }).click();
	const field = page.getByRole('textbox', { name: 'Название' });
	await field.fill('Переименованная сессия');
	await field.press('Enter');

	await expect(page.getByRole('heading', { level: 1 })).toHaveText('Переименованная сессия');
	await expect(page).toHaveTitle('Переименованная сессия · Canterlot');
});

test('chat_tab_title_falls_back_to_the_header_placeholder_without_a_title', async ({ page }) => {
	const { id } = await bootChat(page, '');
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('heading', { level: 1 })).toHaveText('Сессия');
	await expect(page).toHaveTitle('Сессия · Canterlot');
});

test('chat_tab_title_shortens_a_long_title_and_keeps_the_app_name', async ({ page }) => {
	const { id } = await bootChat(page, LONG_TITLE);
	await page.goto(`/c/${id}`);
	await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page.getByRole('heading', { level: 1 })).toHaveText(LONG_TITLE);

	const title = await page.title();
	expect(title.endsWith('… · Canterlot')).toBe(true);
	expect(Array.from(title)).toHaveLength(TAB_TITLE_LIMIT + ' · Canterlot'.length);
});

test('lane_tab_title_puts_the_subagent_before_the_session_title', async ({ page }) => {
	const { id, rootBlockId } = await bootLane(page);
	await page.goto(`/c/${id}/lane/${rootBlockId}`);
	await expect(page.locator('.lane-screen[data-ready]')).toBeVisible({ timeout: 15_000 });
	await expect(page).toHaveTitle('Субагент · Поиск дедупликации · Canterlot');
});

test('route_announcement_still_uses_the_tab_title', async ({ page }) => {
	const { id } = await bootChat(page);
	await page.goto('/sessions');
	await page.getByRole('link', { name: /Сессия Альфа/ }).first().click();
	await expect(page).toHaveURL(new RegExp(`/c/${id}$`));
	await expect(page).toHaveTitle('Сессия Альфа · Canterlot');
	await expect(page.locator('.sr-only[aria-live="polite"]')).toContainText('· Canterlot');
});
