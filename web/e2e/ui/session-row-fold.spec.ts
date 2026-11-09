import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions, type MockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page) {
	const hub = new FakeHub();
	const api = new FakeHubApi(hub);
	const ids = seedMockupSessions(api);
	await installChatRoutes(page, hub, api, OWNER);
	await page.goto('/sessions');
	await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
	return { hub, api, ids: ids as MockupSessions };
}

/** Архив или возврат с другого устройства: строка уходит из списка или приходит в него на глазах. */
function setArchived(hub: FakeHub, api: FakeHubApi, id: string, archived: boolean): void {
	const row = api.list.get(id);
	hub.append('conversations', {
		kind: 'conversation_updated',
		conversation_id: id,
		payload: {
			id, title: row?.title, title_source: 'user', color: row?.color ?? null, pinned: row?.pinned ?? false, archived,
			groups: row?.groups ?? [], machine: row?.machine, harness: row?.harness, workdir: row?.workdir, created_at: row?.created_at, counts: api.counts
		}
	});
	hub.flush();
}

/**
 * Срезы движения: переход места строки ставится на паузу и проматывается на десятую, четверть и половину.
 * Так проверка не зависит от того, сколько кадров успел нарисовать загруженный браузер.
 */
async function slices(page: Page, selector: string, neighbour: string, act: () => unknown): Promise<{ start: number; slot: number[]; below: number[] } | null> {
	// Ожидание и пауза в одном кадре: загруженный браузер иначе успевает доиграть переход между вызовами.
	const waiting = page.evaluate(({ selector }) => new Promise<boolean>((resolve) => {
		const started = performance.now();
		const look = () => {
			// Svelte сперва ставит пустую вводную анимацию нулевой длины: ждём настоящую.
			// Из нескольких движений строки берём то, что меняет высоту: FLIP едет трансформом.
			const folds = (node: Element) => node.getAnimations().filter((animation) => {
				if (!(animation.effect instanceof KeyframeEffect) || Number(animation.effect.getComputedTiming().duration) <= 0) return false;
				return animation.effect.getKeyframes().some((frame) => 'height' in frame || 'gridTemplateRows' in frame);
			});
			const slot = [...document.querySelectorAll(selector)].find((node) => folds(node).length > 0);
			if (slot) {
				for (const animation of folds(slot)) animation.pause();
				(window as unknown as { __slot: Element }).__slot = slot;
				resolve(true);
			} else if (performance.now() - started > 8_000) resolve(false);
			else requestAnimationFrame(look);
		};
		look();
	}), { selector });
	await act();
	if (!(await waiting)) return null;
	return page.evaluate(async ({ neighbour }) => {
		const slot = (window as unknown as { __slot: Element }).__slot;
		const [animation] = slot.getAnimations().filter((item) => item.effect instanceof KeyframeEffect
			&& item.effect.getKeyframes().some((frame) => 'height' in frame || 'gridTemplateRows' in frame));
		const total = Number(animation!.effect!.getComputedTiming().duration);
		animation!.currentTime = 0;
		const out = { start: slot.getBoundingClientRect().height, slot: [] as number[], below: [] as number[] };
		for (const share of [0.1, 0.25, 0.5]) {
			animation!.currentTime = total * share;
			out.slot.push(slot.getBoundingClientRect().height);
			const next = document.querySelector<HTMLElement>(`[data-session="${neighbour}"]`)!;
			const scroller = next.closest<HTMLElement>('[data-list-scroll]')!;
			out.below.push(next.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop);
		}
		animation!.play();
		// Переход доигрывает до конца: иначе уходящая строка ещё в DOM, когда начинается следующее
		// действие, и следующий срез может поймать её затухающую анимацию вместо новой.
		await Promise.race([animation!.finished.catch(() => {}), new Promise((resolve) => setTimeout(resolve, 3_000))]);
		return out;
	}, { neighbour });
}

/** Соседняя строка ниже уходящей: по ней видно, едет список или прыгает. */
async function nextRow(page: Page, id: string, nextSection: boolean): Promise<string> {
	return page.evaluate(({ id, nextSection }) => {
		const all = [...document.querySelectorAll<HTMLElement>('[data-slot]')];
		const index = all.findIndex((node) => node.dataset.slot === id);
		const own = all[index]!.closest('[data-section-slot]');
		return all.slice(index + 1).find((node) => !nextSection || node.closest('[data-section-slot]') !== own)!.dataset.slot!;
	}, { id, nextSection });
}

/** Верх строки в содержимом списка: прокрутка, которую двигает меню строки, на него не влияет. */
const topOf = (page: Page, id: string) => page.locator(`[data-session="${id}"]`).evaluate((el) => {
	const scroller = el.closest<HTMLElement>('[data-list-scroll]')!;
	return el.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
});

for (const motion of ['full', 'reduced'] as const) for (const which of ['pinned', 'plain'] as const) {
	// Закреплённая строка уходит из раздела с соседями; обычная — последняя в своём разделе, и раздел складывается вместе с ней.
	test(`archived_row_folds_and_returned_row_unfolds_in_place_${which}_${motion}`, async ({ page }) => {
		if (motion === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
		const { hub, api, ids } = await boot(page);
		const id = ids[which];
		const slot = which === 'plain' ? `[data-section-slot]:has([data-slot="${id}"])` : `[data-slot="${id}"]`;
		// Уходящая строка теряет опознавательные атрибуты: её место ищется и по следу.
		const trace = which === 'plain'
			? `${slot}, [data-section-slot-leaving]:has([data-slot-leaving="${id}"])`
			: `${slot}, [data-slot-leaving="${id}"]`;
		const row = page.locator(`[data-slot="${id}"]`);
		await expect(row).toBeVisible();
		const height = (await page.locator(slot).boundingBox())!.height;
		const neighbour = await nextRow(page, id, which === 'plain');
		const top = await topOf(page, neighbour);

		const gone = await slices(page, trace, neighbour, () => setArchived(hub, api, id, true));
		await expect(row).toHaveCount(0);
		await expect(page.locator(trace)).toHaveCount(0);
		if (motion === 'reduced') {
			// Без движения строка уходит сразу, сосед встаёт на её место одним шагом.
			expect(gone).toBeNull();
		} else {
			// Высота сходится только вниз, сосед едет следом: на каждом срезе он между прежним и новым местом.
			expect(gone).not.toBeNull();
			const [a, b, c] = gone!.slot;
			expect(a!).toBeLessThan(height - 0.5);
			expect(b!).toBeLessThan(a!);
			expect(c!).toBeLessThan(b!);
			expect(c!).toBeGreaterThanOrEqual(0);
			for (const y of gone!.below) {
				expect(y).toBeGreaterThanOrEqual(top - height - 0.5);
				expect(y).toBeLessThan(top - 0.5);
			}
		}
		await expect.poll(() => topOf(page, neighbour)).toBeCloseTo(top - height, 0);

		const back = await slices(page, trace, neighbour, () => setArchived(hub, api, id, false));
		await expect(row).toBeVisible();
		if (motion === 'reduced') expect(back).toBeNull();
		else {
			expect(back).not.toBeNull();
			const [a, b, c] = back!.slot;
			expect(a!).toBeGreaterThan(0.5);
			expect(b!).toBeGreaterThan(a!);
			expect(c!).toBeGreaterThan(b!);
			expect(c!).toBeLessThan(height - 0.5);
		}
		// Строка вернулась на прежнее место, сосед под ней тоже.
		await expect.poll(() => topOf(page, neighbour)).toBeCloseTo(top, 0);
	});
}

test('leaving_row_is_a_trace_for_readers_and_lookups', async ({ page }) => {
	const { hub, api, ids } = await boot(page);
	await page.evaluate(() => {
		// Медленный переход, чтобы застать след на месте.
		document.documentElement.style.setProperty('--motion-scale', '8');
	});
	setArchived(hub, api, ids.pinned, true);
	const ghost = page.locator(`[data-slot-leaving="${ids.pinned}"]`);
	await expect(ghost).toHaveAttribute('aria-hidden', 'true');
	await expect(page.locator(`[data-session="${ids.pinned}"]`)).toHaveCount(0);
	await expect(page.getByRole('link', { name: /Восстановление потока/ })).toHaveCount(0);
});

test('first_paint_and_tab_switch_build_rows_without_motion', async ({ page }) => {
	await boot(page);
	const moving = () => page.evaluate(() => document.getAnimations().filter((animation) => {
		const target = (animation.effect as KeyframeEffect | null)?.target as Element | null;
		return target?.matches('[data-slot], [data-section-slot]') ?? false;
	}).length);
	expect(await moving()).toBe(0);
	const tabs = page.getByRole('tab');
	await tabs.nth(1).click();
	await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true');
	expect(await moving()).toBe(0);
	await tabs.nth(0).click();
	await expect(tabs.nth(0)).toHaveAttribute('aria-selected', 'true');
	expect(await moving()).toBe(0);
});

const idOf = (page: Page, title: string) => page.getByRole('link', { name: new RegExp(title) }).evaluate((el) => el.closest<HTMLElement>('[data-session]')!.dataset.session!);

async function moveToTrash(page: Page, title: string): Promise<void> {
	await page.getByRole('button', { name: `Действия: ${title}` }).click();
	await page.getByRole('button', { name: /^Переместить в корзину/ }).click();
	await page.getByRole('dialog', { name: 'Переместить сессию в корзину?' }).getByRole('button', { name: 'В корзину', exact: true }).click();
}

function expectFolding(cut: { start: number; slot: number[] } | null): void {
	expect(cut).not.toBeNull();
	const [a, b, c] = cut!.slot;
	expect(a!).toBeLessThan(cut!.start - 0.5);
	expect(b!).toBeLessThan(a!);
	expect(c!).toBeLessThan(b!);
}

function expectGrowing(cut: { slot: number[] } | null, height: number): void {
	expect(cut).not.toBeNull();
	const [a, b, c] = cut!.slot;
	expect(a!).toBeGreaterThan(0.5);
	expect(b!).toBeGreaterThan(a!);
	expect(c!).toBeGreaterThan(b!);
	expect(c!).toBeLessThan(height - 0.5);
}

test('trash_move_undo_restore_and_purge_fold_and_unfold_rows_in_place', async ({ page }) => {
	test.setTimeout(60_000);
	await boot(page);
	const catalog = await idOf(page, 'Каталог');
	const cover = await idOf(page, 'Обложка');
	const below = await nextRow(page, catalog, false);
	const slotOf = (id: string) => `[data-slot="${id}"], [data-slot-leaving="${id}"]`;
	const height = (await page.locator(`[data-slot="${catalog}"]`).boundingBox())!.height;
	const top = await topOf(page, below);

	// «Переместить в корзину»: строка складывается, сосед подтягивается на её место.
	expectFolding(await slices(page, slotOf(catalog), below, () => moveToTrash(page, 'Каталог')));
	await expect.poll(() => topOf(page, below)).toBeCloseTo(top - height, 0);
	// «Вернуть» в подсказке: строка раскрывается там же, сосед возвращается вниз.
	expectGrowing(await slices(page, slotOf(catalog), below, () => page.getByRole('button', { name: 'Вернуть' }).click()), height);
	await expect.poll(() => topOf(page, below)).toBeCloseTo(top, 0);

	// Две сессии в корзине: «Восстановить» складывает строку корзины, «Удалить навсегда» последней — весь раздел.
	await moveToTrash(page, 'Каталог');
	await expect(page.locator(`[data-slot="${catalog}"]`)).toHaveCount(0);
	await moveToTrash(page, 'Обложка');
	await expect(page.getByRole('tab', { name: /Корзина 2/ })).toBeVisible();
	await page.getByRole('tab', { name: /Корзина/ }).click();
	await expect(page.locator(`.trash [data-slot="${cover}"]`)).toBeVisible();
	await expect(page.locator('.trash [data-slot]')).toHaveCount(2);
	const [first, second] = await page.locator('.trash [data-slot]').evaluateAll((nodes) => nodes.map((node) => (node as HTMLElement).dataset.slot!));
	const restoring = await slices(page, slotOf(first!), second!, () => page.locator(`[data-session="${first}"]`).getByRole('button', { name: 'Восстановить' }).click());
	expectFolding(restoring);
	await expect(page.locator('.trash [data-slot]')).toHaveCount(1);

	await page.locator(`[data-session="${second}"]`).getByRole('button', { name: 'Удалить навсегда' }).click();
	const dialog = page.getByRole('dialog', { name: 'Удалить сессию навсегда?' });
	await dialog.getByRole('button', { name: 'Удалить навсегда' }).click();
	await page.waitForFunction(() => [...document.querySelectorAll('[data-section-slot="trash"], [data-section-slot-leaving="trash"]')].some((node) => node.getAnimations().length > 0), null, { timeout: 2_000 });
	await expect(page.getByText('В корзине пусто')).toBeVisible();
	await expect(page.locator('[data-section-slot-leaving="trash"], [data-section-slot="trash"]')).toHaveCount(0);
});
