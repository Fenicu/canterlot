import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, resizeViewport, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

const RETRY = {
  attempt: 2,
  max_retries: 10,
  delay_ms: 3000,
  http_status: 529,
  category: 'overloaded',
  no_response_waited_ms: null
};

async function seed(page: import('@playwright/test').Page, activity: unknown, title: string) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const machine = seedMachine(api, { id: api.id(), name: 'workstation', availability: 'ready' });
  const id = seedConversation(api, {
    title,
    machine: { id: machine.id, name: 'workstation' },
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    model: 'opus-5.5',
    activity: activity as never
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return id;
}

test('retry_row_names_the_reason_and_counts_down_to_zero_without_moving_the_row', async ({ page }) => {
  const since = new Date().toISOString();
  const id = await seed(
    page,
    { state: 'working', evidence: { phase: 'generating', summary: 'retry 2/10', retry: RETRY, phase_since: since }, since },
    'Повтор запроса'
  );
  const row = page.locator(`[data-session="${id}"]`);
  const text = row.locator('.text');
  await expect(text).toContainText('Повторяет запрос к модели: попытка 2 из 10 · перегрузка (529)');
  await expect(text).toContainText(/через [123] с/);
  const before = await row.boundingBox();
  // Отсчёт доходит до нуля и пропадает, строка остаётся той же высоты.
  await expect(text).not.toContainText('через', { timeout: 10_000 });
  await expect(text).toHaveText('Повторяет запрос к модели: попытка 2 из 10 · перегрузка (529)');
  const after = await row.boundingBox();
  expect(after!.height).toBe(before!.height);
});

test('retry_row_fits_the_viewport_and_truncates_with_an_ellipsis_on_a_narrow_screen', async ({ page }, info) => {
  const since = new Date().toISOString();
  const id = await seed(
    page,
    { state: 'working', evidence: { phase: 'generating', summary: 'retry 2/10', retry: { ...RETRY, delay_ms: 600_000 }, phase_since: since }, since },
    'Повтор запроса'
  );
  if (info.project.name === 'phone') await resizeViewport(page, { width: 360, height: 740 });
  const row = page.locator(`[data-session="${id}"]`);
  await expect(row.locator('.text')).toContainText('Повторяет запрос к модели');
  const text = await row.locator('.text').evaluate((node) => ({
    overflow: getComputedStyle(node).textOverflow
  }));
  expect(text.overflow).toBe('ellipsis');
  const page_overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(page_overflow).toBeLessThanOrEqual(0);
  const activity = await row.locator('.activity').evaluate((node) => node.scrollWidth - node.clientWidth);
  expect(activity).toBeLessThanOrEqual(1);
});

test('no_response_row_shows_the_wait_and_the_attempt', async ({ page }) => {
  const since = new Date().toISOString();
  const id = await seed(
    page,
    {
      state: 'working',
      evidence: {
        phase: 'generating',
        summary: 'retry 2/10',
        retry: { ...RETRY, http_status: null, category: 'unknown', delay_ms: 0, no_response_waited_ms: 185_000 },
        phase_since: since
      },
      since
    },
    'Нет ответа'
  );
  await expect(page.locator(`[data-session="${id}"] .text`)).toContainText('Нет ответа 3 мин 05 с · попытка 2 из 10');
});

test('old_hub_summary_without_a_typed_payload_still_gets_a_label', async ({ page }) => {
  const since = new Date().toISOString();
  const id = await seed(page, { state: 'working', evidence: { phase: 'generating', summary: 'retry 2/10' }, since }, 'Старый хаб');
  await expect(page.locator(`[data-session="${id}"] .text`)).toHaveText('Повторяет запрос к модели, попытка 2 из 10');
});
