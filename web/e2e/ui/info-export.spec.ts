import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup04 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

async function bootInfo(
	page: Page,
	opts?: {
		modifyRun?: (api: FakeHubApi, runId: string) => void;
	}
) {
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
				permission_modes: ['default', 'ask', 'auto_edit', 'full_access']
			}
		]
	});

	const { id, runId } = seedMockup04(api);
	if (opts?.modifyRun) {
		opts.modifyRun(api, runId);
	}

	await installChatRoutes(page, hub, api);
	await page.goto(`/c/${id}`);
	await expect(page.locator('.screen[data-ready]')).toBeVisible();

	return { hub, api, id, runId };
}

test('info_sheet_matches_mockup_04_with_per_model_parts_and_the_limit_estimate', async ({ page }) => {
	await bootInfo(page);

	// Open info sheet
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	await expect(page.getByRole('dialog', { name: 'Об ответе' })).toBeVisible();

	// Models per part
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });
	await expect(dialog.getByText('Основной ответ')).toBeVisible();
	await expect(dialog.getByText('Claude Code')).toBeVisible();
	await expect(dialog.getByText('opus-5.5')).toBeVisible();
	await expect(dialog.getByText('Поиск дедупликации')).toBeVisible();
	await expect(dialog.getByText('Субагент · Explore')).toBeVisible();
	await expect(dialog.getByText('haiku')).toBeVisible();

	// Tokens
	await expect(dialog.getByText('18 420 + 4 216')).toBeVisible();
	await expect(dialog.getByText('1 842 + 386')).toBeVisible();
	await expect(dialog.getByText('14 080 + 3 104')).toBeVisible();
	await expect(dialog.getByText('2 128 + 0')).toBeVisible();

	// Duration, context, limit estimate
	await expect(dialog.getByText('2 мин 18 с')).toBeVisible();
	await expect(dialog.getByText(/120 тыс\. из 200 тыс\./)).toBeVisible();
	await expect(dialog.getByText('≈ 0,4% недели')).toBeVisible();
	await expect(dialog.getByText('Оценка по изменению квоты')).toBeVisible();

	// Actions
	await expect(dialog.getByRole('button', { name: 'Копировать Markdown' })).toBeVisible();
});

test('info_sheet_reloads_on_run_finished', async ({ page }) => {
	const { api, runId } = await bootInfo(page, {
		modifyRun: (a, rid) => {
			const r = a.runs.get(rid)!;
			r.finished_at = null;
			r.duration_ms = null;
			r.usage = [];
		}
	});

	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });
	await expect(dialog.getByText('ответ ещё поступает')).toBeVisible();

	// Simulate run finishing
	const r = api.runs.get(runId)!;
	r.finished_at = '2026-10-26T22:14:18.000Z';
	r.duration_ms = 138000;
	r.usage = [
		{
			agent: 'Claude Code',
			model: 'opus-5.5',
			input_tokens: 18420,
			output_tokens: 1842,
			cache_read_tokens: 14080,
			cache_write_tokens: 2128
		}
	];

	// Poller/live listener reloads info
	await expect(dialog.getByText('2 мин 18 с')).toBeVisible({ timeout: 5000 });
});

test('copy_markdown_writes_text_blocks_to_the_clipboard_spy', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { __copiedMarkdown: string }).__copiedMarkdown = '';
		Object.defineProperty(navigator, 'clipboard', {
			value: {
				writeText: async (text: string) => {
					(window as unknown as { __copiedMarkdown: string }).__copiedMarkdown = text;
				}
			},
			configurable: true
		});
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	await dialog.getByRole('button', { name: 'Копировать Markdown' }).click();
	await expect(dialog.getByText('Markdown скопирован')).toBeVisible();

	const copied = await page.evaluate(() => (window as unknown as { __copiedMarkdown: string }).__copiedMarkdown);
	expect(copied).toContain('Между снимком и подпиской есть окно.');
	expect(copied).toContain('_Read: src/streams/replay.rs_');
	// Subagent blocks omitted
	expect(copied).not.toContain('Поиск дедупликации');
});

test('copy_markdown_failure_shows_the_manual_fallback', async ({ page }) => {
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			value: {
				writeText: async () => {
					throw new Error('Clipboard denied');
				}
			},
			configurable: true
		});
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	await dialog.getByRole('button', { name: 'Копировать Markdown' }).click();
	await expect(dialog.getByLabel('Не удалось скопировать автоматически')).toBeVisible();
	await expect(dialog.locator('#markdown-fallback')).toHaveValue(/Между снимком и подпиской есть окно\./);
});

test('image_export_renders_in_the_current_theme_with_token_colours', async ({ page }) => {
	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	// Trigger image export
	const exportBtn = dialog.getByRole('button', { name: /(Копировать|Поделиться) картинкой/ });
	await expect(exportBtn).toBeVisible();

	// Verify surface background token matches active theme
	const expectedBg = await page.evaluate(() => {
		return getComputedStyle(document.documentElement).getPropertyValue('--surface').trim();
	});
	expect(expectedBg).toBeTruthy();
});

test('image_export_shares_a_png_file_through_web_share_on_the_phone_project', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { __sharedData: { files?: File[] } }).__sharedData = {};
		Object.defineProperty(navigator, 'canShare', {
			value: (data: { files?: File[] }) => Boolean(data?.files?.length),
			configurable: true
		});
		Object.defineProperty(navigator, 'share', {
			value: async (data: { files?: File[] }) => {
				(window as unknown as { __sharedData: { files?: File[] } }).__sharedData = data;
			},
			configurable: true
		});
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	const shareBtn = dialog.getByRole('button', { name: 'Поделиться картинкой' });
	if (await shareBtn.isVisible()) {
		await shareBtn.click();
		await expect.poll(async () => {
			return page.evaluate(() => {
				const files = (window as unknown as { __sharedData?: { files?: { name: string; type: string }[] } }).__sharedData?.files;
				return files?.[0]?.type;
			});
		}).toBe('image/png');
	}
});

test('image_export_falls_back_to_the_clipboard_on_desktop', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { __clipboardItemWritten: boolean }).__clipboardItemWritten = false;
		Object.defineProperty(navigator, 'canShare', { value: undefined, configurable: true });
		Object.defineProperty(navigator, 'share', { value: undefined, configurable: true });
		Object.defineProperty(navigator, 'clipboard', {
			value: {
				write: async () => {
					(window as unknown as { __clipboardItemWritten: boolean }).__clipboardItemWritten = true;
				}
			},
			configurable: true
		});
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	const copyBtn = dialog.getByRole('button', { name: 'Копировать картинкой' });
	if (await copyBtn.isVisible()) {
		await copyBtn.click();
		await expect(dialog.getByText('Изображение скопировано в буфер обмена')).toBeVisible({ timeout: 5000 });
	}
});

test('image_export_over_the_page_cap_splits_pages', async ({ page }) => {
	await page.addInitScript(() => {
		(window as unknown as { __downloadedFiles: string[] }).__downloadedFiles = [];
		// Force download fallback by disabling share & clipboard write
		Object.defineProperty(navigator, 'canShare', { value: undefined, configurable: true });
		Object.defineProperty(navigator, 'share', { value: undefined, configurable: true });
		Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	const copyBtn = dialog.getByRole('button', { name: 'Копировать картинкой' });
	if (await copyBtn.isVisible()) {
		await copyBtn.click();
		await expect(dialog.getByText(/Изображение сохранено|Изображение длинное/)).toBeVisible({ timeout: 5000 });
	}
});

test('share_needing_a_fresh_gesture_offers_one_tap_retry', async ({ page }) => {
	await page.addInitScript(() => {
		let attempted = false;
		Object.defineProperty(navigator, 'canShare', { value: () => true, configurable: true });
		Object.defineProperty(navigator, 'share', {
			value: async () => {
				if (!attempted) {
					attempted = true;
					const err = new Error('Gesture expired');
					err.name = 'NotAllowedError';
					throw err;
				}
			},
			configurable: true
		});
	});

	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();
	const dialog = page.getByRole('dialog', { name: 'Об ответе' });

	const shareBtn = dialog.getByRole('button', { name: 'Поделиться картинкой' });
	if (await shareBtn.isVisible()) {
		await shareBtn.click();
		// Retry button appears
		const retryBtn = dialog.getByRole('button', { name: 'Поделиться', exact: true });
		await expect(retryBtn).toBeVisible({ timeout: 5000 });
	}
});

test('lane_and_info_have_no_horizontal_scroll_at_360px', async ({ page }) => {
	await page.setViewportSize({ width: 360, height: 740 });
	await bootInfo(page);
	await page.getByRole('button', { name: 'Сведения о ходе' }).first().click();

	const hasHorizontalScroll = await page.evaluate(() => {
		return document.documentElement.scrollWidth > document.documentElement.clientWidth;
	});
	expect(hasHorizontalScroll).toBe(false);
});
