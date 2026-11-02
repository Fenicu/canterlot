import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const AT = '2026-10-28T12:00:00.000Z';
const LONG_ENGLISH = `${'[canterlot] The worktree of session "Карантин Rust-тестов" could not be removed: git_failed. '.repeat(9)}Конец старого сообщения.`;

async function boot(page: Page) {
  const phone = test.info().project.name === 'phone';
  await page.setViewportSize({ width: phone ? 384 : 1280, height: 844 });
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const parent = seedConversation(api, { title: 'Оркестратор' });
  const child = seedConversation(api, { title: 'Карантин Rust-тестов' });
  const answer = '## Итог\n\nТесты **прошли**, замечаний нет.';

  api.histories.set(child, [
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'ev-1',
      state: 'accepted',
      content: [{ type: 'text', text: '[canterlot] Session "Карантин Rust-тестов" is now idle.' }],
      attachments: [],
      created_seq: '1',
      feed_seq: '1',
      created_at: AT,
      origin: {
        kind: 'event',
        from: child,
        title: 'Карантин Rust-тестов',
        harness: 'claude',
        model: 'opus',
        chain_depth: 1,
        event: {
          kind: 'session_result',
          state: 'idle',
          session_id: child,
          turns: 4,
          duration_ms: 372_000,
          tokens_in: 12_000,
          tokens_out: 900,
          commits: [
            { sha: '5d6e7f8', title: 'Parse the header' },
            { sha: '9a0b1c2', title: 'Add tests' }
          ],
          changed_files: 3,
          answer,
          answer_bytes: answer.length,
          answer_block_id: api.id()
        }
      }
    },
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'ev-2',
      state: 'accepted',
      content: [{ type: 'text', text: '[canterlot] Session "Карантин Rust-тестов" was accepted: it is stopped, archived, and its worktree is removed. Branch canterlot/0190abcd holds commits that exist nowhere else and was kept.' }],
      attachments: [],
      created_seq: '2',
      feed_seq: '2',
      created_at: AT,
      origin: {
        kind: 'event',
        from: child,
        title: 'Карантин Rust-тестов',
        chain_depth: 1,
        event: {
          kind: 'accepted',
          session_id: child,
          worktree: 'removed',
          branch: 'canterlot/0190abcd',
          branch_kept: true
        }
      }
    },
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'ev-3',
      state: 'accepted',
      content: [{ type: 'text', text: '[canterlot] The owner allowed your session_spawn request 00000000-0000-4000-8000-0000000000e1. Session 00000000-0000-4000-8000-0000000000e2 started.' }],
      attachments: [],
      created_seq: '3',
      feed_seq: '3',
      created_at: AT,
      origin: {
        kind: 'event',
        title: 'Миграция схемы',
        chain_depth: 1,
        event: {
          kind: 'approval',
          decision: 'allowed',
          request_id: '00000000-0000-4000-8000-0000000000e1',
          child_id: '00000000-0000-4000-8000-0000000000e2'
        }
      }
    },
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'ev-4',
      state: 'accepted',
      content: [{ type: 'text', text: '[canterlot] Session "Сборка" is now stopped.' }],
      attachments: [],
      created_seq: '4',
      feed_seq: '4',
      created_at: AT,
      origin: {
        kind: 'event',
        from: child,
        title: 'Сборка',
        chain_depth: 1,
        event: { kind: 'stopped', state: 'stopped', session_id: child, outcome: 'owner_stop' }
      }
    },
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'ev-5',
      state: 'accepted',
      content: [{ type: 'text', text: LONG_ENGLISH }],
      attachments: [],
      created_seq: '5',
      feed_seq: '5',
      created_at: AT,
      origin: { kind: 'event', title: 'Старая сессия', chain_depth: 1 }
    },
    {
      kind: 'input',
      id: api.id(),
      idempotency_key: 'owner-1',
      state: 'accepted',
      content: [{ type: 'text', text: 'Моё сообщение' }],
      attachments: [],
      created_seq: '6',
      feed_seq: '6',
      created_at: AT
    }
  ] as never);
  api.bindConversation(child);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${child}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, child, parent };
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
}

test('session_result_is_a_full_width_card_with_expandable_answer_and_link', async ({ page }) => {
  const { child } = await boot(page);
  const card = page.locator('[data-event-kind="session_result"]');
  await expect(card).toBeVisible();
  await expect(page.locator('article[data-origin="event"]')).toHaveCount(0);
  await expect(card).not.toContainText('[canterlot]');

  await expect(card.getByRole('link', { name: 'Карантин Rust-тестов' })).toBeVisible();
  await expect(card.getByText('завершена')).toBeVisible();
  for (const [label, value] of [
    ['ходы', '4'],
    ['время', '6 мин 12 с'],
    ['коммиты', '2'],
    ['файлы', '3']
  ]) {
    await expect(card.getByText(label, { exact: true })).toBeVisible();
    await expect(card.getByText(value, { exact: true })).toBeVisible();
  }
  await expect(card.getByText('Итог')).toHaveCount(0);
  await card.getByRole('button', { name: 'Показать ответ' }).click();
  await expect(card.getByText('Итог')).toBeVisible();
  await expect(card.getByText('прошли', { exact: false })).toBeVisible();
  await expect(card.getByRole('button', { name: 'Скрыть ответ' })).toBeVisible();
  await noOverflow(page);

  const toggle = page.locator('.chat-header [data-theme-toggle]');
  if ((await page.locator('html').getAttribute('data-theme')) !== 'light') await toggle.click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await noOverflow(page);
  await expect(card).toBeVisible();
  if ((await page.locator('html').getAttribute('data-theme')) === 'light') await toggle.click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await noOverflow(page);

  await card.getByRole('link', { name: 'Карантин Rust-тестов' }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${child}$`));
});

test('small_hub_events_are_quiet_lines_without_the_english_text', async ({ page }) => {
  await boot(page);
  const accepted = page.locator('[data-event-kind="accepted"]');
  await expect(accepted).toContainText(
    '«Карантин Rust-тестов» принята, рабочая копия убрана, ветка canterlot/0190abcd оставлена'
  );
  const approval = page.locator('[data-event-kind="approval"]');
  await expect(approval).toContainText('Запуск «Миграция схемы» разрешён');
  const stopped = page.locator('[data-event-kind="stopped"]');
  await expect(stopped).toContainText('«Сборка» остановлена');
  for (const line of [accepted, approval, stopped]) {
    await expect(line).not.toContainText('[canterlot]');
    await expect(line).not.toContainText('was accepted');
    await expect(line).not.toContainText('session_spawn');
  }
  await expect(page.locator('article[data-origin="event"]')).toHaveCount(0);
  await noOverflow(page);
});

test('old_hub_rows_stay_simple_and_collapsed', async ({ page }) => {
  await boot(page);
  const legacy = page.locator('[data-event-kind="legacy"]');
  await expect(legacy).toBeVisible();
  await expect(legacy.getByText('Сообщение хаба')).toBeVisible();
  await expect(legacy.getByText('Конец старого сообщения.')).toHaveCount(0);
  await legacy.getByRole('button', { name: 'Показать текст' }).click();
  await expect(legacy.getByText('Конец старого сообщения.')).toBeVisible();
  await expect(legacy.getByRole('button', { name: 'Скрыть текст' })).toBeVisible();
  await expect(page.locator('article[data-origin="event"]')).toHaveCount(0);
  await noOverflow(page);
});

test('owner_message_keeps_its_bubble_and_delivery_status', async ({ page }) => {
  await boot(page);
  const bubble = page.locator('article.bubble').filter({ hasText: 'Моё сообщение' });
  await expect(bubble).toBeVisible();
  await expect(bubble).toContainText('Принято агентом');
  await noOverflow(page);
});
