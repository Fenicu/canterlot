import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup04 } from '../../testing/fixtures/mockup-chat';
import { expect, test } from './fixtures';

const SHOTS = '/tmp/cost-line-shots';

function boot() {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [{ id: 'claude', title: 'Claude Code', provider: 'claude', models: [{ id: 'opus-5.5', title: 'opus-5.5', efforts: ['high'] }], permission_modes: ['owner', 'bypass'] }]
  });
  const { id, runId } = seedMockup04(api);
  Object.assign(api.runs.get(runId)!, {
    limit: { provider: 'claude', weekly_delta_permille: 14, exact: true },
    usage: [
      { agent: 'orchestrator', model: 'claude-opus-5-5', input_tokens: 2000, output_tokens: 1000, cache_read_tokens: 45000, cache_write_tokens: 0, cost_micro_usd: 620_000, cost_source: 'cli' },
      { agent: 'Explore', model: 'claude-haiku-5-5', input_tokens: 4000, output_tokens: 400, cache_read_tokens: 0, cache_write_tokens: 0, cost_micro_usd: 10_000, cost_source: 'cli' }
    ]
  });
  return { hub, api, id, runId };
}

test('feed_line_under_a_finished_turn_shows_model_tokens_cache_money_and_week_and_taps_into_the_sheet', async ({ page }) => {
  const { hub, api, id } = boot();
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  const line = page.locator('[data-role="run-cost"]').first();
  await expect(line).toHaveText('Opus 5.5 · 48 тыс. токенов (кэш 96%) · ≈ $0,62 + Haiku 5.5 ≈ $0,01 · ≈ 1,4% недели');
  const box = await line.boundingBox();
  const viewport = page.viewportSize()!;
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width + 0.5);
  await line.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/feed-line-${test.info().project.name}.png` });

  await line.click();
  const sheet = page.getByRole('dialog', { name: 'Об ответе' });
  const costs = sheet.locator('[data-role="run-costs"]');
  await expect(costs).toContainText('Opus 5.5');
  await expect(costs).toContainText('≈ $0,62');
  await expect(costs).toContainText('Haiku 5.5');
  await expect(costs).toContainText('по данным CLI');
  await costs.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/info-sheet-${test.info().project.name}.png` });
});

test('feed_line_updates_when_the_cost_of_a_finished_turn_arrives_late', async ({ page }) => {
  const { hub, api, id, runId } = boot();
  api.runs.get(runId)!.usage = [{ agent: 'orchestrator', model: 'claude-opus-5-5', input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 0 }];
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await expect(page.locator('[data-role="run-cost"]')).toHaveCount(0);

  hub.append(`conversation:${id}`, {
    kind: 'run_cost_updated',
    conversation_id: id,
    payload: {
      run_id: runId,
      usage: [{ model: 'mystery-model', agent: 'orchestrator', input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 0, cost_micro_usd: null, cost_source: null }],
      limit: null,
      cache_hint: null
    }
  });
  hub.flush();
  await expect(page.locator('[data-role="run-cost"]').first()).toHaveText('mystery-model · 15 токенов (кэш 0%) · ≈ $ —');

  hub.append(`conversation:${id}`, {
    kind: 'run_cost_updated',
    conversation_id: id,
    payload: {
      run_id: runId,
      usage: [{ model: 'opencode-go/beam-free', agent: 'orchestrator', input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 0, cost_micro_usd: 0, cost_source: 'cli' }],
      limit: null,
      cache_hint: null
    }
  });
  hub.flush();
  await expect(page.locator('[data-role="run-cost"]').first()).toHaveText('beam-free · 15 токенов (кэш 0%) · бесплатно');
});

test('session_sheet_shows_the_session_total_with_delegates_and_turns_without_a_price', async ({ page }) => {
  const { hub, api, id } = boot();
  api.usage.set(id, {
    session: { turns: 3, unknown_cost_turns: 1, cost_micro_usd: 1_000_000, input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 0, models: [] },
    delegates: { turns: 2, unknown_cost_turns: 1, cost_micro_usd: 250_000, input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, models: [] }
  });
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: 'Диагностика' }).click();
  const total = page.getByRole('dialog', { name: 'Диагностика' }).locator('[data-role="session-cost"]');
  await expect(total).toContainText('≈ $1,25 за сессию с делегатами');
  await expect(total).toContainText('2 хода без цены');
  await total.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/session-sheet-${test.info().project.name}.png` });
});

test('feed_line_shows_the_cache_hint_without_overflow_or_a_broken_line', async ({ page }) => {
  const { hub, api, id, runId } = boot();
  Object.assign(api.runs.get(runId)!, { cache_hint: { kind: 'expired', idle_minutes: 125 } });
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible();

  const line = page.locator('[data-role="run-cost"]').first();
  const hint = line.locator('[data-role="cache-hint"]');
  await expect(hint).toHaveText('кэш, вероятно, истёк · простой 2 ч 5 мин');
  await expect(line).toContainText('Opus 5.5 · 48 тыс. токенов (кэш 96%)');
  const viewport = page.viewportSize()!;
  const box = await line.boundingBox();
  expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width + 0.5);
  const hintBox = await hint.boundingBox();
  expect(hintBox!.x + hintBox!.width).toBeLessThanOrEqual(viewport.width + 0.5);
  // Подсказка на одной строке: многоточие вместо переноса.
  const lineHeight = await hint.evaluate((node) => parseFloat(getComputedStyle(node).lineHeight) || node.getBoundingClientRect().height);
  expect(hintBox!.height).toBeLessThanOrEqual(lineHeight + 1);
  await line.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/feed-line-cache-hint-${test.info().project.name}.png` });

  hub.append(`conversation:${id}`, {
    kind: 'run_cost_updated',
    conversation_id: id,
    payload: {
      run_id: runId,
      usage: [{ model: 'claude-opus-5-5', agent: 'orchestrator', input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 9000, cost_micro_usd: 5000, cost_source: 'cli' }],
      limit: null,
      cache_hint: { kind: 'model_changed', idle_minutes: null }
    }
  });
  hub.flush();
  await expect(page.locator('[data-role="cache-hint"]').first()).toHaveText('смена модели — кэш заново');
});

test('feed_line_has_no_hint_when_the_hub_names_none', async ({ page }) => {
  const { hub, api, id } = boot();
  await installChatRoutes(page, hub, api);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-role="run-cost"]').first()).toBeVisible();
  await expect(page.locator('[data-role="cache-hint"]')).toHaveCount(0);
});
