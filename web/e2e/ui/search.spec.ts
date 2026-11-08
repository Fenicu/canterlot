import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { DEEP_TEXT, seedSearch, type SearchSeed } from '../../testing/fixtures/search';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page, path = '/sessions', ready = '[data-list-ready]') {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const seed = seedSearch(api);
	await installChatRoutes(page, hub, api, OWNER);
	// Адрес каждой навигации: запрос не должен попасть ни в один.
	const urls: string[] = [];
	page.on('framenavigated', (frame) => {
		if (frame === page.mainFrame()) urls.push(frame.url());
	});
	await page.goto(path);
	await expect(page.locator(ready).first()).toBeVisible({ timeout: 15_000 });
	return { hub, api, seed, urls };
}

function desktop(page: Page): boolean {
	return (page.viewportSize()?.width ?? 0) >= 74 * 16;
}

/** Экран поиска на телефоне или панель стола. */
function view(page: Page) {
	return page.locator('[data-search-view]');
}

async function searchFromList(page: Page, q: string): Promise<void> {
	await page.getByRole('button', { name: 'Поиск', exact: true }).click();
	await page.getByRole('textbox', { name: 'Поиск сессий' }).fill(q);
	const deep = page.locator('[data-deep-search]');
	await expect(deep).toHaveText(`Искать в сообщениях: “${q}”`);
	await deep.click();
	await expect(view(page)).toBeVisible();
}

function expectNoQuery(urls: string[], page: Page, q: string): void {
	for (const url of [...urls, page.url()]) {
		expect(new URL(url).searchParams.has('q'), url).toBe(false);
		expect(decodeURIComponent(url).toLowerCase(), url).not.toContain(q.toLowerCase());
	}
}

async function expectHighlighted(page: Page, seed: SearchSeed): Promise<void> {
	const block = page.locator(`[data-block="${seed.deepBlockId}"]`);
	await expect(block).toHaveText(DEEP_TEXT, { timeout: 10_000 });
	await expect(block).toBeInViewport();
	await expect(page.locator('[data-row][data-jump]')).toContainText(DEEP_TEXT);
}

test('search_from_the_list_finds_answers_and_messages_and_jumps_to_a_deep_hit', async ({ page }) => {
	const { api, seed, urls } = await boot(page);
	await searchFromList(page, 'индекс');
	if (desktop(page)) await expect(page.locator('[data-pane="lane"] [data-search-panel]')).toBeVisible();
	else await expect(page).toHaveURL(/\/search$/);

	await expect(page.locator('[data-search-total]')).toHaveText('Найдено 6 совпадений');
	const hits = page.locator('[data-search-hit]');
	await expect(hits).toHaveCount(6);
	// Свежие первыми: своё сообщение минуту назад, затем ответ глубоко в ленте.
	await expect(hits.nth(0)).toContainText('Миграция поиска');
	await expect(hits.nth(0).locator('[data-search-origin]')).toHaveText('моё сообщение');
	await expect(hits.nth(0).locator('mark')).toHaveText('индекс');
	await expect(hits.nth(1).locator('[data-search-origin]')).toHaveText('ответ');
	await expect(hits.nth(1)).toContainText('Claude · opus-5.5');
	const archived = page.locator('[data-search-hit]', { hasText: 'Ревью схемы' });
	await expect(archived.locator('[data-search-archived]')).toHaveText('в архиве');
	await expect(archived).toContainText('Codex');
	const origins = await page.locator('[data-search-origin]').allTextContents();
	expect(origins).toEqual(expect.arrayContaining(['итог', 'сообщение сессии']));
	expect(api.searchCalls.at(-1)).toMatchObject({ q: 'индекс', limit: 20 });

	// Прыжок к ответу далеко от хвоста: окно ленты встаёт вокруг seq, блок подсвечен.
	const historyCalls: string[] = [];
	page.on('request', (request) => {
		if (request.url().includes('/history')) historyCalls.push(request.url());
	});
	await hits.nth(1).click();
	await expect(page).toHaveURL(new RegExp(`/c/${seed.migration}\\?block=${seed.deepBlockId}&seq=${seed.deepSeq}$`));
	await expectHighlighted(page, seed);
	expect(historyCalls.some((url) => new URL(url).searchParams.get('around') === String(seed.deepSeq))).toBe(true);
	expectNoQuery(urls, page, 'индекс');

	if (desktop(page)) {
		// Панель стола остаётся открытой рядом с лентой: соседнее совпадение открывается тем же нажатием.
		await expect(view(page).getByRole('searchbox', { name: 'Поиск по сообщениям' })).toHaveValue('индекс');
		await hits.nth(0).click();
		await expect(page).toHaveURL(new RegExp(`/c/${seed.migration}\\?input=${seed.ownerInputId}&seq=161$`));
		await expect(page.locator('.bubble[data-jump]')).toContainText('индекс строится', { timeout: 10_000 });
	} else {
		// «Назад» возвращает поиск с тем же запросом из записи истории, а не из адреса.
		await page.goBack();
		await expect(page).toHaveURL(/\/search$/);
		await expect(view(page).getByRole('searchbox', { name: 'Поиск по сообщениям' })).toHaveValue('индекс');
		await expect(page.locator('[data-search-hit]')).toHaveCount(6);
		expectNoQuery(urls, page, 'индекс');
	}
});

test('search_in_this_session_scopes_by_a_chip_that_can_be_removed', async ({ page }) => {
	const { api, seed } = await boot(page, undefined, '[data-list-ready]');
	await page.goto(`/c/${seed.migration}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Искать в этой сессии' }).click();
	await expect(view(page)).toBeVisible();
	await expect(page.locator('[data-search-scope]')).toContainText('В сессии «Миграция поиска»');
	await expect(page.locator('[data-search-empty]')).toContainText('в этой сессии');

	await view(page).getByRole('searchbox', { name: 'Поиск по сообщениям' }).fill('индекс');
	await expect(page.locator('[data-search-hit]')).toHaveCount(2);
	expect(api.searchCalls.at(-1)).toMatchObject({ q: 'индекс', conversation_id: seed.migration });
	await expect(page.locator('[data-search-hit]', { hasText: 'Ревью схемы' })).toHaveCount(0);

	await page.getByRole('button', { name: 'Искать во всех сессиях' }).click();
	await expect(page.locator('[data-search-scope]')).toHaveCount(0);
	await expect(page.locator('[data-search-hit]')).toHaveCount(6);
	expect(api.searchCalls.at(-1)?.conversation_id).toBeUndefined();
});

test('period_and_kind_filters_narrow_the_results', async ({ page }) => {
	const { api } = await boot(page);
	await searchFromList(page, 'индекс');
	await expect(page.locator('[data-search-hit]')).toHaveCount(6);

	await view(page).getByRole('radiogroup', { name: 'Период' }).getByText('Неделя').click();
	await expect(page.locator('[data-search-hit]')).toHaveCount(3);
	expect(api.searchCalls.at(-1)?.since).toBeTruthy();

	await view(page).getByRole('radiogroup', { name: 'Что искать' }).getByText('Сообщения').click();
	await expect(page.locator('[data-search-hit]')).toHaveCount(1);
	expect(api.searchCalls.at(-1)).toMatchObject({ kind: 'messages' });

	await view(page).getByRole('radiogroup', { name: 'Период' }).getByText('Всё').click();
	await expect(page.locator('[data-search-hit]')).toHaveCount(4);
	expect(await page.locator('[data-search-origin]').allTextContents()).not.toContain('ответ');

	await view(page).getByRole('radiogroup', { name: 'Что искать' }).getByText('Ответы').click();
	await expect(page.locator('[data-search-hit]')).toHaveCount(2);
	expect(new Set(await page.locator('[data-search-origin]').allTextContents())).toEqual(new Set(['ответ']));
});

test('results_load_more_pages_on_scroll_until_the_cursor_ends', async ({ page }) => {
	const { api } = await boot(page);
	await searchFromList(page, 'проверка');
	await expect(page.locator('[data-search-total]')).toHaveText('Найдено 45 совпадений');
	const hits = page.locator('[data-search-hit]');
	await expect(hits).toHaveCount(20);
	const results = page.locator('[data-search-results]');
	await results.evaluate((node) => { node.scrollTop = node.scrollHeight; });
	await expect(hits).toHaveCount(40);
	await results.evaluate((node) => { node.scrollTop = node.scrollHeight; });
	await expect(hits).toHaveCount(45);
	await results.evaluate((node) => { node.scrollTop = node.scrollHeight; });
	await page.waitForTimeout(300);
	expect(api.searchCalls.filter((call) => call.q === 'проверка').map((call) => call.cursor ?? null)).toEqual([null, 'o:20', 'o:40']);
	await expect(hits.first()).toContainText('Проверка 45');
	await expect(hits.last()).toContainText('Проверка 1');
});

test('empty_query_nothing_found_and_timeout_states', async ({ page }) => {
	const { api, seed } = await boot(page);
	await page.goto(`/c/${seed.journal}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
	await page.getByRole('button', { name: 'Меню сессии' }).click();
	await page.getByRole('menuitem', { name: 'Искать в этой сессии' }).click();
	await page.getByRole('button', { name: 'Искать во всех сессиях' }).click();
	await expect(page.locator('[data-search-empty]')).toContainText('Поиск по ответам и сообщениям во всех сессиях');
	expect(api.searchCalls).toHaveLength(0);

	const field = view(page).getByRole('searchbox', { name: 'Поиск по сообщениям' });
	await field.fill('индекс.rs');
	await expect(page.locator('[data-search-nothing]')).toContainText('Ничего не найдено');
	await expect(page.locator('[data-search-nothing]')).toContainText('Части слов, пути к файлам и фрагменты кода не находятся');

	api.searchFailure = { status: 503, code: 'internal', message: 'timeout' };
	await field.fill('индекс');
	const failure = page.locator('[data-search-error="timeout"]');
	await expect(failure).toContainText('Поиск не уложился во время');
	api.searchFailure = null;
	await failure.getByRole('button', { name: 'Повторить' }).click();
	await expect(page.locator('[data-search-hit]')).toHaveCount(6);

	await field.press('Escape');
	await expect(field).toHaveValue('');
	await expect(page.locator('[data-search-empty]')).toBeVisible();
});
