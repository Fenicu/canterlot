import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

interface Booted {
  hub: FakeHub;
  api: FakeHubApi;
  id: string;
}

async function boot(
  page: Page,
  options: {
    title: string;
    at: number;
    activity: Record<string, unknown>;
    kind: 'permission' | 'question';
    payload: Record<string, unknown>;
  }
): Promise<Booted> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const workstation = seedMachine(api, { id: api.id(), name: 'workstation', availability: 'ready' });
  seedMachine(api, { id: api.id(), name: 'mini', availability: 'ready' });
  const id = seedConversation(api, {
    title: options.title,
    machine: { id: workstation.id, name: 'workstation' },
    harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' },
    model: 'opus-5.5',
    unread_count: 2,
    activity: options.activity as never
  });
  const detail = api.details.get(id) as Record<string, unknown>;
  detail.activity = options.activity;
  detail.machine = { id: workstation.id, name: 'workstation' };
  detail.execution = { state: 'running', generation: 1 };
  hub.append(`conversation:${id}`, {
    kind: 'request_opened',
    conversation_id: id,
    execution_id: '00000000-0000-4000-8000-0000000000e1',
    at: new Date(options.at).toISOString(),
    payload: { request_id: '00000000-0000-4000-8000-0000000000f1', kind: options.kind, payload: options.payload }
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function permissionActivity(createdAt: number): Record<string, unknown> {
  return {
    state: 'waiting_for_user',
    evidence: { phase: 'awaiting_permission', question: 'Bash: cargo test' },
    since: new Date(createdAt).toISOString()
  };
}

test('permission_wait_names_the_tool_and_the_age_in_row_header_and_card', async ({ page }) => {
  const at = Date.now() - 12 * 60_000;
  const { id } = await boot(page, {
    title: 'Сборка хаба',
    at,
    activity: permissionActivity(at),
    kind: 'permission',
    payload: { tool_name: 'Bash', command: 'cargo test' }
  });

  await expect(page.locator('.state-chip .state-text')).toHaveText('Ждёт разрешения · Bash · 12 мин');
  const head = page.locator('.permission-card .card-head .meta');
  await expect(head).toHaveText('Запрос разрешения · 12 мин назад');
  // Дольше десяти минут — предупреждение не только словом, но и тоном.
  await expect(head).toHaveAttribute('data-wait-long', '');
  const warning = await page.evaluate(() => {
    const probe = document.createElement('span');
    probe.style.color = 'var(--warning)';
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  });
  expect(await head.evaluate((node) => getComputedStyle(node).color)).toBe(warning);

  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const row = page.locator(`[data-session="${id}"]`);
  await expect(row.locator('.text')).toHaveText('Ждёт разрешения · Bash · 12 мин');
});

test('question_wait_counts_the_variants_and_stays_calm_before_ten_minutes', async ({ page }) => {
  const at = Date.now() - 3 * 60_000;
  const { id } = await boot(page, {
    title: 'Выбор палитры',
    at,
    activity: {
      state: 'waiting_for_user',
      evidence: {
        phase: 'awaiting_answer',
        question: 'Какой порог?',
        request_kind: 'question',
        option_count: 4,
        request_created_at: new Date(at).toISOString()
      },
      since: new Date(at).toISOString()
    },
    kind: 'question',
    payload: {
      questions: [
        {
          id: 'q1',
          text: 'Какой порог?',
          options: ['20%', '15%', '10%', '5%']
        }
      ]
    }
  });

  await expect(page.locator('.state-chip .state-text')).toHaveText('Вопрос · 4 варианта · 3 мин');
  const head = page.locator('.question-card .card-head .meta');
  await expect(head).toHaveText('Вопрос · 3 мин назад');
  await expect(head).not.toHaveAttribute('data-wait-long');

  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const row = page.locator(`[data-session="${id}"]`);
  await expect(row.locator('.text')).toHaveText('Вопрос · 4 варианта · 3 мин');
});
