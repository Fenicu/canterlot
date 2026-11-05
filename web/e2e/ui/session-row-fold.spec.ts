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
async function slices(page: Page, selector: string, neighbour: string, act: () => void): Promise<{ slot: number[]; below: number[] } | null> {
	act();
	const found = await page
		.waitForFunction((selector) => [...document.querySelectorAll(selector)].some((slot) => slot.getAnimations().length > 0), selector, { timeout: 1_500 })
		.then(() => true, () => false);
	if (!found) return null;
	return page.evaluate(({ selector, neighbour }) => {
		const slot = [...document.querySelectorAll(selector)].find((node) => node.getAnimations().length > 0)!;
		const [animation] = slot.getAnimations();
		animation!.pause();
		const total = Number(animation!.effect!.getComputedTiming().duration);
		const out = { slot: [] as number[], below: [] as number[] };
		for (const share of [0.1, 0.25, 0.5]) {
			animation!.currentTime = total * share;
			out.slot.push(slot.getBoundingClientRect().height);
			out.below.push(document.querySelector(`[data-session="${neighbour}"]`)!.getBoundingClientRect().top);
		}
		animation!.play();
		return out;
	}, { selector, neighbour });
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

const topOf = (page: Page, id: string) => page.locator(`[data-session="${id}"]`).evaluate((el) => el.getBoundingClientRect().top);

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
	expect(await moving()).toBe(0);
	await tabs.nth(0).click();
	expect(await moving()).toBe(0);
});
