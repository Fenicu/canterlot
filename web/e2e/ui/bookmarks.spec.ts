import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import {
	CARD_LAST_TEXT,
	DEEP_BOOKMARK_TEXT,
	OWNER_BOOKMARK_TEXT,
	seedBookmarks,
	type BookmarksSeed
} from '../../testing/fixtures/bookmarks';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const CARD_KEY = 'answer:bookmark-card';

async function boot(
	page: Page,
	options: { path?: string; global?: boolean } = {}
): Promise<{ hub: FakeHub; api: FakeHubApi; seed: BookmarksSeed }> {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const seed = seedBookmarks(api, { global: options.global });
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto(options.path ?? '/sessions');
	await expect(page.locator('[data-list-ready]').first()).toBeVisible({ timeout: 15_000 });
	return { hub, api, seed };
}

async function openChat(page: Page, id: string): Promise<void> {
	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
}

function answerCard(page: Page) {
	return page.locator(`[data-row][data-key="${CARD_KEY}"]`);
}

async function openSessionBookmarks(page: Page, count: number | null = null): Promise<void> {
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page
		.getByRole('menuitem', { name: count === null ? /^Избранное/ : `Избранное · ${count}` })
		.click();
}

async function noOverflow(page: Page): Promise<boolean> {
	return page.evaluate(() => {
		const doc = document.documentElement;
		return doc.scrollWidth <= doc.clientWidth && document.body.scrollWidth <= doc.clientWidth;
	});
}

async function seedMainBookmarks(api: FakeHubApi, seed: BookmarksSeed): Promise<void> {
	const at = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();
	api.putBookmark(seed.main, 'block', seed.deepBlockId, { at: at(30), emit: false });
	api.putBookmark(seed.main, 'input', seed.ownerInputId, { at: at(20), emit: false });
	api.putBookmark(seed.main, 'block', seed.cardLastId, { at: at(10), emit: false });
}

test('bookmark_toggle_on_an_answer_sends_the_anchor_and_flips_back_on_failure', async ({ page }) => {
	const { api, seed } = await boot(page);
	await openChat(page, seed.main);
	const card = answerCard(page);
	await expect(card.getByRole('button', { name: 'В избранное' })).toHaveAttribute('aria-pressed', 'false');

	await card.getByRole('button', { name: 'В избранное' }).click();
	await expect(card.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');
	expect(api.bookmarkCalls.at(-1)).toMatchObject({
		method: 'PUT',
		kind: 'block',
		target: seed.cardLastId
	});

	await card.getByRole('button', { name: 'Убрать из избранного' }).click();
	await expect(card.getByRole('button', { name: 'В избранное' })).toBeVisible();
	expect(api.bookmarkCalls.at(-1)).toMatchObject({
		method: 'DELETE',
		kind: 'block',
		target: seed.cardLastId
	});

	api.bookmarkFailure = { status: 500, code: 'internal', message: 'Закладка не сохранилась' };
	await card.getByRole('button', { name: 'В избранное' }).click();
	await expect(page.locator('.bookmark-error')).toContainText('Внутренняя ошибка хаба');
	await expect(card.getByRole('button', { name: 'В избранное' })).toHaveAttribute('aria-pressed', 'false');
	api.bookmarkFailure = null;

	// Отметка на другом верхнем блоке тоже делает карточку отмеченной: снятие убирает её.
	api.putBookmark(seed.main, 'block', seed.cardFirstId);
	await expect(card.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');
	await card.getByRole('button', { name: 'Убрать из избранного' }).click();
	await expect(card.getByRole('button', { name: 'В избранное' })).toBeVisible();
	expect(api.bookmarkCalls.at(-1)).toMatchObject({
		method: 'DELETE',
		kind: 'block',
		target: seed.cardFirstId
	});
});

test('bookmark_toggle_on_an_owner_message_targets_the_input', async ({ page }) => {
	const { api, seed } = await boot(page);
	await openChat(page, seed.main);
	const bubble = page.locator('.bubble').filter({ hasText: OWNER_BOOKMARK_TEXT });
	const toggle = bubble.getByRole('button', { name: 'В избранное' });
	const status = bubble.locator('[data-role="status-row"] .status');
	const statusBox = (await status.boundingBox())!;
	const toggleBox = (await toggle.boundingBox())!;
	// Закладка делит строку со статусом и временем, а не делает пузырь выше на целую строку.
	expect(toggleBox.y + toggleBox.height / 2).toBeGreaterThanOrEqual(statusBox.y);
	expect(toggleBox.y + toggleBox.height / 2).toBeLessThanOrEqual(statusBox.y + statusBox.height);
	const rowBox = (await bubble.locator('[data-role="status-row"]').boundingBox())!;
	expect(rowBox.height).toBeLessThanOrEqual(statusBox.height + 1);
	await toggle.click();
	await expect(bubble.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');
	expect(api.bookmarkCalls.at(-1)).toMatchObject({
		method: 'PUT',
		kind: 'input',
		target: seed.ownerInputId
	});
});

test('session_sheet_lists_in_timeline_order_and_jumps_like_a_search_hit', async ({ page }) => {
	const { api, seed } = await boot(page);
	await seedMainBookmarks(api, seed);
	await openChat(page, seed.main);
	await openSessionBookmarks(page, 3);

	const sheet = page.getByRole('dialog', { name: 'Избранное' });
	await expect(sheet).toBeVisible();
	const rows = sheet.locator('[data-bookmark-row]');
	await expect(rows).toHaveCount(3);
	await expect(rows.nth(0).locator('[data-bookmark-excerpt]')).toHaveText(DEEP_BOOKMARK_TEXT);
	await expect(rows.nth(1).locator('[data-bookmark-excerpt]')).toHaveText(OWNER_BOOKMARK_TEXT);
	await expect(rows.nth(2).locator('[data-bookmark-excerpt]')).toHaveText(CARD_LAST_TEXT);

	const cardRow = rows.filter({ hasText: CARD_LAST_TEXT });
	await cardRow.getByRole('button', { name: 'Убрать из избранного' }).click();
	await expect(rows).toHaveCount(2);
	await sheet.getByRole('button', { name: 'Закрыть' }).click();
	await expect(answerCard(page).getByRole('button', { name: 'В избранное' })).toBeVisible();

	await openSessionBookmarks(page, 2);
	const historyCalls: string[] = [];
	page.on('request', (request) => {
		if (request.url().includes('/history')) historyCalls.push(request.url());
	});
	await page.locator('.feed').evaluate((node) => node.dispatchEvent(new WheelEvent('wheel', { deltaY: -100 })));
	await page
		.locator('[data-bookmark-row]')
		.filter({ hasText: DEEP_BOOKMARK_TEXT })
		.getByRole('button', { name: /^Открыть сообщение/ })
		.click();
	await expect(sheet).toHaveCount(0);
	await expect(page.locator(`[data-block="${seed.deepBlockId}"]`)).toBeInViewport({ timeout: 10_000 });
	await expect(page.locator('[data-row][data-jump]')).toContainText(DEEP_BOOKMARK_TEXT);
	expect(
		historyCalls.some((url) => new URL(url).searchParams.get('around') === String(seed.deepSeq))
	).toBe(true);
});

test('all_bookmarks_page_pages_by_cursor_opens_the_message_and_removes_rows', async ({ page }) => {
	const { api, seed } = await boot(page);
	await page.getByRole('button', { name: 'Избранное' }).click();
	await expect(page).toHaveURL(/\/bookmarks$/);
	await expect(page.locator('[data-bookmarks-screen]')).toBeVisible();
	await expect(page.locator('[data-bookmarks-screen]')).toHaveAttribute('data-ready', '');

	const rows = page.locator('[data-bookmark-row]');
	await expect(rows).toHaveCount(20);
	await expect(rows.nth(0)).toContainText('Журнал закладок');
	await expect(rows.nth(0).locator('[data-bookmark-excerpt]')).toHaveText('Запись 1: журнал проверок и закладок');
	await expect(rows.nth(19).locator('[data-bookmark-excerpt]')).toHaveText('Запись 20: журнал проверок и закладок');

	await page.locator('[data-bookmarks-results]').evaluate((node) => {
		node.dispatchEvent(new WheelEvent('wheel', { deltaY: 600 }));
		node.scrollTop = node.scrollHeight;
	});
	await expect(rows).toHaveCount(23);
	expect(api.bookmarkPages.at(-1)?.cursor).toBeTruthy();

	const archived = rows.filter({ hasText: 'Ревью схемы' });
	await expect(archived.locator('[data-bookmark-archived]')).toHaveText('в архиве');
	await archived.getByRole('button', { name: /^Открыть сообщение/ }).click();
	await expect(page).toHaveURL(
		new RegExp(`/c/${seed.archived}\\?block=${seed.archivedBlockId}&seq=1$`)
	);
	await expect(page.locator(`[data-block="${seed.archivedBlockId}"]`)).toBeInViewport();

	await page.goBack();
	await expect(page).toHaveURL(/\/bookmarks$/);
	await expect(page.locator('[data-bookmark-row]')).toHaveCount(20);
	const first = page.locator('[data-bookmark-row]').first();
	await expect(first).toContainText('Журнал закладок');
	await first.getByRole('button', { name: 'Убрать из избранного' }).click();
	await expect(page.locator('[data-bookmark-row]')).toHaveCount(19);
	expect(api.hasBookmark(seed.journal, 'block', seed.journalBlockIds[0]!)).toBe(false);
});

test('empty_bookmarks_screen_says_where_bookmarks_come_from', async ({ page }) => {
	await boot(page, { global: false });
	await page.getByRole('button', { name: 'Избранное' }).click();
	await expect(page.locator('[data-bookmarks-empty]')).toHaveText(
		'Здесь появятся сообщения, отмеченные закладкой в чате.'
	);
});

test('bookmark_changed_updates_the_message_counts_and_both_lists_without_a_reload', async ({ page }) => {
	const { api, seed } = await boot(page, { global: false });
	await openChat(page, seed.main);
	const card = answerCard(page);

	// Своё нажатие и собственное событие не удваивают запись.
	await card.getByRole('button', { name: 'В избранное' }).click();
	await expect(card.getByRole('button', { name: 'Убрать из избранного' })).toBeVisible();
	await page.waitForTimeout(120);
	await openSessionBookmarks(page, 1);
	await expect(page.locator('[data-bookmark-row]')).toHaveCount(1);
	await page.getByRole('dialog', { name: 'Избранное' }).getByRole('button', { name: 'Закрыть' }).click();

	// Другое устройство добавляет закладку в тот же разговор: счётчик и лист уже на экране.
	api.putBookmark(seed.main, 'input', seed.ownerInputId);
	await openSessionBookmarks(page, 2);
	await expect(page.locator('[data-bookmark-row]')).toHaveCount(2);
	await page.getByRole('dialog', { name: 'Избранное' }).getByRole('button', { name: 'Закрыть' }).click();

	await page.goBack();
	await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
	await page.getByRole('button', { name: 'Избранное' }).click();
	const rows = page.locator('[data-bookmark-row]');
	await expect(rows).toHaveCount(2);
	await expect(rows.first()).toContainText('Миграция поиска');

	// Чужая закладка встаёт наверх без перезагрузки.
	api.putBookmark(seed.archived, 'block', seed.archivedBlockId);
	await expect(rows).toHaveCount(3);
	await expect(rows.first()).toContainText('Ревью схемы');

	// Чужое удаление убирает строку; своё нажатие не оставляет дубля.
	api.deleteBookmark(seed.main, 'block', seed.cardLastId);
	await expect(rows).toHaveCount(2);
	await expect(rows.first()).toContainText('Ревью схемы');
});

test('phone_widths_keep_chat_toggles_sheet_and_bookmarks_page_inside_the_viewport', async ({ page }, testInfo) => {
	test.skip(testInfo.project.name !== 'phone', 'телефонные ширины');
	for (const width of [320, 412]) {
		await page.setViewportSize({ width, height: 800 });
		const { api, seed } = await boot(page);
		await seedMainBookmarks(api, seed);
		await openChat(page, seed.main);
		await expect(answerCard(page).getByRole('button', { name: 'Убрать из избранного' })).toBeVisible();
		expect(await noOverflow(page), `чат ${width}`).toBe(true);

		await openSessionBookmarks(page, 3);
		await expect(page.getByRole('dialog', { name: 'Избранное' })).toBeVisible();
		expect(await noOverflow(page), `лист ${width}`).toBe(true);
		await page.getByRole('dialog', { name: 'Избранное' }).getByRole('button', { name: 'Закрыть' }).click();

		await page.goBack();
		await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
		await page.getByRole('button', { name: 'Избранное' }).click();
		await expect(page.locator('[data-bookmark-row]').first()).toBeVisible();
		expect(await noOverflow(page), `/bookmarks ${width}`).toBe(true);
	}
});
