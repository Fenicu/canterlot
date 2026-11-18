import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation, seedMachine, seedPresets } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

const CATALOG = {
  id: 'workstation',
  name: 'workstation',
  harnesses: [{
    id: 'codex', title: 'Codex', provider: 'codex',
    models: [{ id: 'mini', title: 'mini', efforts: ['low', 'medium', 'high'] }],
    permission_modes: ['owner', 'ask', 'bypass', 'read_only']
  }]
};

function goal(status: string) {
  return {
    objective: 'Подготовить промежуточный отчёт',
    status,
    tokens_used: 1200,
    time_used_seconds: 180,
    token_budget: 5000,
    turns_used: 3,
    updated_at: '2026-11-18T12:00:00.000Z'
  };
}

interface Boot {
  hub: FakeHub;
  api: FakeHubApi;
  id: string;
}

async function boot(page: Page, harnessGoals = true): Promise<Boot> {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, CATALOG as never);
  seedPresets(api);
  const id = seedConversation(api, { title: 'Сессия' });
  const detail = api.details.get(id)! as Record<string, unknown>;
  detail.harness = { id: 'codex', title: 'Codex', provider: 'codex', goals: harnessGoals };
  detail.machine = { id: 'workstation', name: 'workstation' };
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hub.subscribed(`conversation:${id}`)).toBe(true);
  return { hub, api, id };
}

interface GoalHit { key: string; body: Record<string, unknown> }

async function watchGoal(page: Page, api: FakeHubApi): Promise<GoalHit[]> {
  const hits: GoalHit[] = [];
  await page.route('**/api/v1/conversations/*/goal', async (route) => {
    hits.push({
      key: route.request().headers()['idempotency-key'] ?? '',
      body: JSON.parse(route.request().postData() ?? '{}') as Record<string, unknown>
    });
    await route.fulfill({
      status: 200,
      json: { requested: { command_id: api.id() } },
      headers: { 'X-Server-Time-Ms': String(Date.now()) }
    });
  });
  return hits;
}

test('composer sends the draft as a goal, not as a message', async ({ page }) => {
  const { api } = await boot(page);
  const hits = await watchGoal(page, api);
  const messages: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().includes('/inputs')) messages.push(request.url());
  });
  const text = page.getByRole('textbox', { name: 'Сообщение' });
  await text.fill('Подготовить промежуточный отчёт');
  await page.getByRole('button', { name: 'Отправить как цель' }).click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0].body).toEqual({ action: 'set', objective: 'Подготовить промежуточный отчёт' });
  expect(hits[0].key).toMatch(/^[A-Za-z0-9_-]{1,256}$/);
  expect(messages, 'goal text must not become an ordinary message').toEqual([]);
  await expect(text).toHaveValue('');
});

test('goal row shows every status in words and colour and updates live', async ({ page }) => {
  const { hub, id } = await boot(page);
  const row = page.locator('[data-role="goal-state-line"]');
  await expect(row).toHaveCount(0);
  for (const [status, word, tone] of [
    ['active', 'активна', 'success'],
    ['paused', 'на паузе', 'warning'],
    ['blocked', 'заблокирована', 'danger'],
    ['usage_limited', 'упёрлась в лимит', 'warning'],
    ['budget_limited', 'бюджет исчерпан', 'danger'],
    ['complete', 'достигнута', 'success']
  ]) {
    hub.append(`conversation:${id}`, {
      kind: 'goal_changed',
      conversation_id: id,
      payload: { conversation_id: id, goal: goal(status) }
    });
    hub.flush();
    await expect(row).toBeVisible();
    await expect(row).toContainText(word);
    await expect(row).toHaveAttribute('data-tone', tone);
    await expect(row).toContainText('Подготовить промежуточный отчёт');
    const line = await row.locator('.goal-objective').evaluate((node) => ({
      height: node.getBoundingClientRect().height,
      scroll: node.scrollHeight,
      overflow: getComputedStyle(node).textOverflow
    }));
    expect(line.scroll, 'objective stays on one line').toBeLessThanOrEqual(line.height + 1);
    expect(line.overflow).toBe('ellipsis');
  }
});

test('goal details stop the goal with an idempotent command', async ({ page }) => {
  const { hub, api, id } = await boot(page);
  const hits = await watchGoal(page, api);
  hub.append(`conversation:${id}`, {
    kind: 'goal_changed',
    conversation_id: id,
    payload: { conversation_id: id, goal: goal('active') }
  });
  hub.flush();
  await page.locator('[data-role="goal-state-line"]').click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('[data-role="goal-objective"]')).toHaveText('Подготовить промежуточный отчёт');
  await expect(dialog.locator('[data-role="goal-usage"]')).toContainText('токенов');
  const stop = dialog.locator('[data-role="goal-stop"]');
  await stop.click();
  await expect(stop).toHaveText('Точно остановить?');
  expect(hits, 'first tap only arms the confirmation').toEqual([]);
  await stop.click();
  await expect.poll(() => hits.length).toBe(1);
  expect(hits[0].body).toEqual({ action: 'clear' });
  await expect(dialog).toBeHidden();

  hub.append(`conversation:${id}`, {
    kind: 'goal_changed',
    conversation_id: id,
    payload: { conversation_id: id, goal: null }
  });
  hub.flush();
  await expect(page.locator('[data-role="goal-state-line"]')).toHaveCount(0);
});

test('goal controls are hidden without harness support', async ({ page }) => {
  const { hub, api, id } = await boot(page, false);
  await expect(page.getByRole('button', { name: 'Отправить как цель' })).toHaveCount(0);
  hub.append(`conversation:${id}`, {
    kind: 'goal_changed',
    conversation_id: id,
    payload: { conversation_id: id, goal: goal('active') }
  });
  hub.flush();
  await expect(page.locator('[data-role="goal-state-line"]')).toHaveCount(0);
  expect(api).toBeTruthy();
});
