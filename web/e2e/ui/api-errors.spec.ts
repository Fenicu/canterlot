import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import {
  ADAPTER_TEXT,
  OVERLOADED_TEXT,
  seedApiErrors,
  VERIFY_TEXT,
  VERIFY_URL,
  type ApiErrorSeed
} from '../../testing/fixtures/api-errors';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

async function boot(page: Page): Promise<ApiErrorSeed & { api: FakeHubApi }> {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 412 : 1280, height: 900 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const id = seedConversation(api, { title: 'Ошибки провайдера' });
  const seed = seedApiErrors(api, id);
  api.bindConversation(id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { ...seed, api };
}

function apiBubble(page: Page, title: string) {
  return page.locator('article.system[data-event-kind="api_error"]').filter({ hasText: title });
}

/** Контраст текста элемента с ближайшим непрозрачным фоном под ним, по формуле WCAG. */
function textContrast(locator: ReturnType<typeof apiBubble>) {
  return locator.evaluate((node) => {
    const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
    const luminance = (parts: number[]) =>
      parts
        .slice(0, 3)
        .map((c) => {
          const s = c / 255;
          return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
        })
        .reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i]!, 0);
    const own = luminance(rgb(getComputedStyle(node).color));
    let parent = node.parentElement;
    let under = luminance(rgb(getComputedStyle(document.documentElement).backgroundColor));
    while (parent) {
      const value = getComputedStyle(parent).backgroundColor;
      const parts = rgb(value);
      if (parts.length >= 3 && (parts[3] ?? 1) > 0) {
        under = luminance(parts);
        break;
      }
      parent = parent.parentElement;
    }
    return (Math.max(own, under) + 0.05) / (Math.min(own, under) + 0.05);
  });
}

test('a_403_verification_error_is_its_own_bubble_with_the_provider_link_and_folded_text', async ({ page }) => {
  const seed = await boot(page);
  const bubble = apiBubble(page, 'Claude: аккаунт требует проверки');
  await bubble.scrollIntoViewIfNeeded();
  await expect(bubble).toHaveAttribute('data-tone', 'danger');
  // Вид отказа назван словами: цвет не единственный носитель состояния.
  await expect(bubble).toHaveAttribute('aria-label', 'Claude: аккаунт требует проверки');
  await expect(bubble).toContainText('аккаунт требует проверки');
  await expect(bubble).toContainText(
    'Провайдер не отвечает этому аккаунту, пока он не пройдёт проверку. После проверки отправьте сообщение снова.'
  );

  const link = bubble.getByRole('link', { name: 'Открыть страницу провайдера' });
  await expect(link).toHaveAttribute('href', VERIFY_URL);
  await expect(link).toHaveAttribute('target', '_blank');
  await expect(link).toHaveAttribute('rel', /noopener/);
  await expect(link).toHaveAttribute('rel', /noreferrer/);
  // Ссылка и пояснение читаются на мягкой заливке: не меньше AA.
  expect(await textContrast(link)).toBeGreaterThanOrEqual(4.5);
  expect(await textContrast(bubble.locator('.reason'))).toBeGreaterThanOrEqual(4.5);

  const details = bubble.getByRole('button', { name: 'Подробнее' });
  await expect(details).toHaveAttribute('aria-expanded', 'false');
  await expect(bubble.locator('[data-role="system-body"]')).toHaveCount(0);
  await details.click();
  await expect(details).toHaveAttribute('aria-expanded', 'true');
  const body = bubble.locator('[data-role="system-body"]');
  await expect(body).toHaveText(VERIFY_TEXT);
  // Исходный текст — вторичная подробность того же размера, что и пояснение.
  const reasonSize = await bubble.locator('.reason').evaluate((node) => getComputedStyle(node).fontSize);
  await expect(body).toHaveCSS('font-size', reasonSize);
  // И простой текст: ни ссылок, ни разметки провайдера.
  await expect(body.locator('a, strong, code, h1, h2, h3')).toHaveCount(0);

  // Ошибка не повторяется уведомлением в карточке ответа, и пустой карточки не осталось.
  const answers = page.locator('[data-row][data-kind="answer"]');
  await expect(answers).toHaveCount(1);
  await expect(answers.first()).toContainText('Проверил окружение и остановился.');
  await expect(answers).not.toContainText('API Error');

  // Закладка переехала на блок ошибки.
  const bookmark = bubble.getByRole('button', { name: 'В избранное' });
  await expect(bookmark).toHaveAttribute('aria-pressed', 'false');
  await bookmark.click();
  await expect(bubble.getByRole('button', { name: 'Убрать из избранного' })).toHaveAttribute('aria-pressed', 'true');
  expect(seed.api.bookmarkCalls.at(-1)).toMatchObject({
    method: 'PUT',
    kind: 'block',
    target: seed.verifyBlock
  });

  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('a_529_overload_is_a_warning_bubble_without_an_action_link', async ({ page }) => {
  await boot(page);
  const bubble = apiBubble(page, 'Claude перегружен');
  await bubble.scrollIntoViewIfNeeded();
  await expect(bubble).toHaveAttribute('data-tone', 'warning');
  await expect(bubble).toHaveAttribute('aria-label', 'Claude перегружен');
  await expect(bubble).toContainText('Сервер провайдера перегружен. Повторите запрос позже.');
  await expect(bubble.getByRole('link', { name: 'Открыть страницу провайдера' })).toHaveCount(0);
  await bubble.getByRole('button', { name: 'Подробнее' }).click();
  await expect(bubble.locator('[data-role="system-body"]')).toHaveText(OVERLOADED_TEXT);
});

test('a_non_api_error_stays_a_notice_inside_the_answer_card', async ({ page }) => {
  await boot(page);
  const card = page.locator('[data-row][data-kind="answer"]');
  const notice = card.locator('.notice[data-kind="error"]');
  await expect(notice).toContainText('Ошибка');
  await expect(notice).toContainText(ADAPTER_TEXT);
  // Такие отказы адаптера не становятся пузырями провайдера.
  await expect(page.locator('article.system[data-event-kind="api_error"]')).toHaveCount(2);
});

test('the_api_error_bubbles_do_not_overflow_and_keep_distinct_names', async ({ page }) => {
  await boot(page);
  for (const width of [320, 412]) {
    await page.setViewportSize({ width, height: 900 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    for (const bubble of await page.locator('article.system[data-event-kind="api_error"]').all()) {
      const names = await bubble.locator('a,button').evaluateAll((nodes) =>
        nodes.map((node) => (node.getAttribute('aria-label') || node.textContent || '').trim())
      );
      for (const name of names) expect(name, `пустое имя в ${width}px`).not.toBe('');
      expect(new Set(names).size, `повтор имени в ${width}px: ${names.join(', ')}`).toBe(names.length);
    }
  }
});
