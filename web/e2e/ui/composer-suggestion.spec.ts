import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedHistory } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { stubMicrophone } from './dictation-fixture';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d3';
const PLACEHOLDER = 'Сообщение агенту…';
const SUGGESTION = 'добавь тесты для пустой строки';
const CLAUDE = { id: 'claude_code', title: 'Claude Code', provider: 'claude' };

interface Boot {
  suggestion?: string | null;
  draft?: string;
  harness?: typeof CLAUDE;
  working?: boolean;
  /** Токены контекста; окно без значения — доля неизвестна. */
  tokens?: number;
  window?: number | null;
  /** Микрофон включён: его кнопка занимает место в нижней строке. */
  mic?: boolean;
}

async function boot(page: Page, opts: Boot = {}) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { harness: opts.harness ?? CLAUDE });
  seedHistory(api, id, 2);
  if (opts.working) api.details.get(id)!.activity = { state: 'working', evidence: { phase: 'generating' }, since: new Date().toISOString() };
  if (opts.suggestion !== null) api.setPromptSuggestion(id, opts.suggestion ?? SUGGESTION);
  if (opts.draft) api.putDraft(id, { text: opts.draft });
  if (opts.tokens !== undefined) api.pushContext(id, opts.tokens, opts.window ?? null);
  if (opts.mic) {
    hub.dictation.status = { available: true, reason: null, mode: 'gpu', engine: 'warm' };
    await stubMicrophone(page, 'granted');
    await page.addInitScript(() => localStorage.setItem('canterlot.dictation', 'on'));
  }
  const posts: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && /\/api\/v1\/conversations\/[^/]+\/inputs$/.test(request.url())) posts.push(request.url());
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  const area = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(area).toBeVisible();
  return { hub, api, id, area, posts };
}

interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

async function boxOf(page: Page, selector: string): Promise<Box | null> {
  const locator = page.locator(selector).first();
  if ((await locator.count()) === 0) return null;
  const box = await locator.boundingBox();
  return box ? { x: box.x, y: box.y, width: box.width, height: box.height } : null;
}

test('suggestion_shows_only_in_the_empty_field_and_typing_hides_it', async ({ page }) => {
  const { api, id, area } = await boot(page);
  await expect(area).toHaveAttribute('placeholder', SUGGESTION);
  await expect(area).toHaveValue('');
  // Подсказка не текст поля и не черновик.
  expect(api.draft(id).text).toBe('');
  await area.click();
  await page.keyboard.type('д');
  await expect(area).toHaveAttribute('placeholder', PLACEHOLDER);
  await expect(page.getByRole('button', { name: 'Подставить' })).toHaveCount(0);
  await page.keyboard.press('Backspace');
  await expect(area).toHaveValue('');
  await expect(area).toHaveAttribute('placeholder', SUGGESTION);
  await expect.poll(() => api.draft(id).text).toBe('');
});

test('draft_wins_over_the_suggestion', async ({ page }) => {
  const { area } = await boot(page, { draft: 'мой черновик' });
  await expect(area).toHaveValue('мой черновик');
  await expect(area).toHaveAttribute('placeholder', PLACEHOLDER);
  await expect(page.getByRole('button', { name: 'Подставить' })).toHaveCount(0);
});

test('suggestion_is_described_for_screen_readers', async ({ page }) => {
  const { area } = await boot(page);
  const described = await area.getAttribute('aria-describedby');
  const ids = (described ?? '').split(' ').filter(Boolean);
  expect(ids.length).toBe(2);
  await expect(page.locator(`[id="${ids[1]}"]`)).toContainText(`Подсказка: ${SUGGESTION}`);
});

test('tab_accepts_the_suggestion_on_desktop', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'Tab — клавиатура компьютера');
  const { api, id, area } = await boot(page);
  await area.focus();
  await page.keyboard.press('Tab');
  await expect(area).toHaveValue(SUGGESTION);
  await expect(area).toBeFocused();
  expect(await area.evaluate((node: HTMLTextAreaElement) => node.selectionStart)).toBe(SUGGESTION.length);
  // Принятая подсказка — обычный черновик.
  await expect.poll(() => api.draft(id).text, { timeout: 10_000 }).toBe(SUGGESTION);
  // Подсказки больше не видно: Tab снова уводит фокус.
  await page.keyboard.press('Tab');
  await expect(area).not.toBeFocused();
  await expect(area).toHaveValue(SUGGESTION);
});

test('tab_keeps_focus_order_without_a_suggestion', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'Tab — клавиатура компьютера');
  const { area } = await boot(page, { suggestion: null });
  await area.focus();
  await page.keyboard.press('Tab');
  await expect(area).not.toBeFocused();
  await expect(area).toHaveValue('');
});

test('button_accepts_the_suggestion_on_phone', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'кнопка только на сенсорном экране');
  const { api, id, area } = await boot(page);
  const accept = page.getByRole('button', { name: 'Подставить' });
  await expect(accept).toBeVisible();
  await accept.click();
  await expect(area).toHaveValue(SUGGESTION);
  await expect(area).toBeFocused();
  await expect(accept).toHaveCount(0);
  await expect.poll(() => api.draft(id).text, { timeout: 10_000 }).toBe(SUGGESTION);
});

test('button_does_not_cover_the_context_meter_or_the_send_button', async ({ page }, info) => {
  test.skip(info.project.name !== 'phone', 'кнопка только на сенсорном экране');
  await boot(page, { tokens: 170_000, window: 1_000_000, mic: true });
  const accept = page.getByRole('button', { name: 'Подставить' });
  await expect(accept).toBeVisible();
  const acceptBox = await boxOf(page, '[data-suggestion-accept]');
  const sendBox = await boxOf(page, '[data-composer] .send');
  expect(acceptBox).not.toBeNull();
  expect(sendBox).not.toBeNull();
  expect(overlaps(acceptBox!, sendBox!)).toBe(false);

  // Счётчик либо уступает место подсказке, либо целиком виден рядом с ней.
  if (await page.locator('[data-composer] .ctx').count()) {
    for (const selector of ['[data-composer] .ctx', '[data-composer] .ctx .label', '[data-composer] .ctx [role=meter]']) {
      const marker = await boxOf(page, selector);
      if (marker) expect(overlaps(acceptBox!, marker)).toBe(false);
    }
  }

  // Уступка временная: принятая подсказка освобождает строку, и счётчик возвращается.
  await accept.click();
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveValue(SUGGESTION);
  await expect(page.locator('[data-composer] .ctx')).toBeVisible();
});

test('enter_on_an_empty_field_with_a_suggestion_sends_nothing', async ({ page }) => {
  const { area, posts } = await boot(page);
  await area.focus();
  await page.keyboard.press('Enter');
  // Кнопка помечена недоступной: щелчок доходит до обработчика, но ничего не уходит.
  await page.getByRole('button', { name: 'Отправить', exact: true }).dispatchEvent('click');
  await page.waitForTimeout(300);
  expect(posts).toEqual([]);
  await expect(area).toHaveValue(/^\n?$/);
});

test('suggestion_updates_live_and_clears_with_the_event', async ({ page }) => {
  const { api, id, area } = await boot(page, { suggestion: null });
  await expect(area).toHaveAttribute('placeholder', PLACEHOLDER);
  api.setPromptSuggestion(id, 'запусти сборку');
  await expect(area).toHaveAttribute('placeholder', 'запусти сборку');
  api.setPromptSuggestion(id, null);
  await expect(area).toHaveAttribute('placeholder', PLACEHOLDER);
});

test('sending_clears_the_suggestion', async ({ page }) => {
  const { area } = await boot(page);
  await area.fill('своё сообщение');
  await page.getByRole('button', { name: 'Отправить', exact: true }).click();
  await expect(area).toHaveValue('');
  await expect(area).toHaveAttribute('placeholder', PLACEHOLDER);
});

test('no_suggestion_for_other_harnesses_or_while_a_turn_runs', async ({ page }) => {
  const codex = await boot(page, { harness: { id: 'codex', title: 'Codex', provider: 'codex' } });
  await expect(codex.area).toHaveAttribute('placeholder', PLACEHOLDER);
  const working = await boot(page, { working: true });
  await expect(working.area).toHaveAttribute('placeholder', 'Ход идёт: сообщение встанет в очередь');
});

test('turning_the_setting_off_hides_the_suggestion', async ({ page }) => {
  const { api, id, area } = await boot(page);
  await expect(area).toHaveAttribute('placeholder', SUGGESTION);
  await page.goto('/settings?c=input');
  const section = page.locator('[data-prompt-suggestions]');
  await expect(section.getByRole('heading', { name: 'Подсказки следующего сообщения' })).toBeVisible({ timeout: 15_000 });
  await section.getByText('Выключены').click();
  await expect.poll(() => api.promptSuggestionsEnabled).toBe(false);
  // Выключенная настройка не сохраняет новых подсказок.
  api.setPromptSuggestion(id, 'новая');
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('textbox', { name: 'Сообщение' })).toHaveAttribute('placeholder', PLACEHOLDER);
});
