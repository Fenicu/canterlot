import type { Page } from '@playwright/test';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const LONG_SUBJECT = 'Очень длинная тема записи про миграции тестовой базы данных в изолированной рабочей копии перед выпуском';
const LONG_BODY = 'Текст записи, который сильно длиннее строки карточки и должен переноситься по словам, а не уходить за её правый край. '.repeat(4);

// Правый край любого текстового узла внутри строки не выходит за правый край самой строки.
async function overflowing(page: Page, rows: string) {
  return page.evaluate((selector) => {
    const out: string[] = [];
    for (const row of document.querySelectorAll(selector)) {
      const edge = row.getBoundingClientRect().right;
      const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (!node.textContent?.trim()) continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        const right = Math.max(0, ...[...range.getClientRects()].map((rect) => rect.right));
        if (right > edge + 0.5) out.push(`${node.parentElement?.tagName}.${node.parentElement?.className}: ${Math.round(right)} > ${Math.round(edge)}`);
      }
    }
    return out;
  }, rows);
}

test('memory_revision_rows_wrap_text_inside_the_row', async ({ page }) => {
  await page.setViewportSize({ width: 1920, height: 1000 });
  await bootSecondary(page, '/settings?c=memory', (api) => {
    const seeded = api.seedMemory({ domain: 'work', subject: LONG_SUBJECT.repeat(3), body: LONG_BODY });
    api.updateMemory(seeded.id, { domain: 'work', expected_version: 1, subject: 'Короткая тема' });
  });
  await page.locator('[data-memory-row]').first().click();
  const sheet = page.getByRole('dialog', { name: 'Запись' });
  await sheet.getByText('Прошлые версии').click();
  await expect(sheet.locator('[data-role="memory-revisions"] button')).toHaveCount(1);
  expect(await overflowing(page, '[data-role="memory-revisions"] button'), 'текст за правым краем строки версии').toEqual([]);
});

for (const width of [1280, 1920, 2440]) {
  test(`memory_rows_wrap_text_inside_the_card_at_${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await bootSecondary(page, '/settings?c=memory', (api) => {
      api.seedMemory({ domain: 'work', subject: LONG_SUBJECT, body: LONG_BODY, kind: 'gotcha' });
      api.seedMemory({ domain: 'work', subject: 'Короткая', body: 'Коротко' });
    });
    await expect(page.locator('[data-memory-row]').first()).toBeVisible({ timeout: 15_000 });
    const rows = '[data-memory-row]';
    expect(await page.locator(rows).count()).toBeGreaterThan(1);
    expect(await overflowing(page, rows), 'текст за правым краем строки').toEqual([]);
  });
}
