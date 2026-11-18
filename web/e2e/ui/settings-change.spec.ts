import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedConversation } from '../../testing/fake-hub/seed';
import { expect, test } from './fixtures';

const OWNER = '00000000-0000-4000-8000-0000000000d1';

// Вымышленные нейтральные значения: никаких путей, узлов, проектов и навыков.
const ROUTING = {
  setting: 'routing',
  reason: 'Ускорить обработку коротких задач',
  base_revision: 3,
  before: {
    task_types: {
      short: {
        title: 'Короткая задача',
        candidates: [
          { harness: 'h1', model: 'm1', effort: 'low' },
          { harness: 'h3', model: 'm3' }
        ]
      }
    },
    providers: {},
    max_parallel: {},
    unlimited_models: [],
    unknown_limits: 'allow'
  },
  after: {
    task_types: {
      short: {
        title: 'Короткая задача',
        candidates: [
          { harness: 'h1', model: 'm1', effort: 'high' },
          { harness: 'h2', model: 'm2' }
        ]
      }
    },
    providers: {},
    max_parallel: {},
    unlimited_models: [],
    unknown_limits: 'allow'
  }
};

const WHITELIST = {
  setting: 'whitelist',
  reason: 'Разрешить ещё одного исполнителя',
  base_revision: 5,
  before: [{ harness: 'h1', model: 'm1' }],
  after: [
    { harness: 'h1', model: 'm1' },
    { harness: 'h2', model: 'default' }
  ]
};

const PRESET = {
  setting: 'preset',
  preset_id: 'p1',
  title: 'Правила работы',
  reason: 'Уточнить порядок шагов',
  base_revision: 2,
  before: 'первая строка\nвторая строка',
  after: 'первая строка\nтретья строка'
};

function openEvent(requestId: string, kind: string, payload: unknown) {
  return {
    kind: 'request_opened',
    payload: { request_id: requestId, kind, payload }
  };
}

async function bootChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, {
    id: 'workstation',
    name: 'workstation',
    harnesses: [
      {
        id: 'agent-a',
        title: 'Агент',
        provider: 'codex',
        models: [{ id: 'model-a', title: 'модель', efforts: ['low', 'medium', 'high'] }],
        permission_modes: ['owner', 'ask', 'bypass', 'read_only']
      }
    ]
  });
  const id = seedConversation(api, {
    title: 'Сессия настроек',
    model: 'model-a',
    activity: { state: 'idle', evidence: {}, since: new Date().toISOString() }
  });
  const detail = api.details.get(id);
  if (detail) {
    detail.harness = { id: 'agent-a', title: 'Агент', provider: 'codex' };
    detail.machine = { id: 'workstation', name: 'workstation' };
    detail.model = 'model-a';
    detail.effort = 'medium';
    detail.permission_mode = 'owner';
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api, id };
}

function open(hub: FakeHub, id: string, requestId: string, kind: string, payload: unknown) {
  hub.append(`conversation:${id}`, openEvent(requestId, kind, payload));
  hub.flush();
}

function close(hub: FakeHub, id: string, requestId: string, reason: string) {
  hub.append(`conversation:${id}`, {
    kind: 'request_closed',
    payload: { request_id: requestId, reason }
  });
  hub.flush();
}

test('card_shows_the_reason_and_human_readable_difference_for_each_setting_kind', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  open(hub, id, api.id(), 'settings_change', ROUTING);
  open(hub, id, api.id(), 'settings_change', WHITELIST);
  open(hub, id, api.id(), 'settings_change', PRESET);

  const routing = page.locator('[data-settings-change][data-setting="routing"]');
  await expect(routing).toBeVisible();
  await expect(routing).toContainText('Ускорить обработку коротких задач');
  await expect(routing).toContainText('Усилие: низкое → высокое');
  await expect(routing).toContainText('Добавлен кандидат h2/m2');
  await expect(routing).toContainText('Без изменений');

  const whitelist = page.locator('[data-settings-change][data-setting="whitelist"]');
  await expect(whitelist).toBeVisible();
  await expect(whitelist).toContainText('Разрешить ещё одного исполнителя');
  await expect(whitelist).toContainText('Добавлена запись h2/модель по умолчанию');

  const preset = page.locator('[data-settings-change][data-setting="preset"]');
  await expect(preset).toBeVisible();
  await expect(preset).toContainText('Инструкции: Правила работы');
  await expect(preset).toContainText('Уточнить порядок шагов');
  await expect(preset.locator('[data-mark="added"]')).toContainText('+ третья строка');
  await expect(preset.locator('[data-mark="removed"]')).toContainText('− вторая строка');

  await expect(page.locator('[data-settings-change]')).toHaveCount(3);
});

test('approve_applies_the_change_and_decline_closes_it', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  const allowed = api.id();
  open(hub, id, allowed, 'settings_change', ROUTING);
  await page.locator('[data-settings-change] button[data-action="allow"]').click();
  await expect(page.locator('[data-outcome="применено"]')).toBeVisible();

  api.decisionAutoProgress = false;
  const declined = api.id();
  open(hub, id, declined, 'settings_change', WHITELIST);
  await page.locator('[data-setting="whitelist"] button[data-action="deny"]').click();
  close(hub, id, declined, 'declined');
  await expect(page.locator('[data-outcome="отклонено"]')).toBeVisible();
});

test('stale_base_revision_and_expiry_have_their_own_words', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  api.decisionAutoProgress = false;

  const stale = api.id();
  open(hub, id, stale, 'settings_change', PRESET);
  await page.locator('[data-setting="preset"] button[data-action="allow"]').click();
  close(hub, id, stale, 'setting_changed');
  await expect(page.locator('[data-outcome="настройка изменилась"]')).toBeVisible();

  const expired = api.id();
  open(hub, id, expired, 'settings_change', ROUTING);
  close(hub, id, expired, 'expired');
  await expect(page.locator('[data-outcome="истекло"]')).toBeVisible();
});

test('decision_from_another_page_updates_the_first_page', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  api.decisionAutoProgress = false;
  const requestId = api.id();
  open(hub, id, requestId, 'settings_change', ROUTING);

  const second = await page.context().newPage();
  await installChatRoutes(second, hub, api, OWNER);
  await second.goto(`/c/${id}`);
  await expect(second.locator('[data-settings-change]')).toBeVisible();

  await second.locator('[data-settings-change] button[data-action="allow"]').click();
  await expect(page.locator('[data-settings-change] button[data-action="allow"]')).toBeDisabled();
  await expect(page.locator('.st-status')).toHaveText('сохранено хабом');

  close(hub, id, requestId, 'allowed');
  await expect(page.locator('[data-outcome="применено"]')).toBeVisible();
  await expect(second.locator('[data-outcome="применено"]')).toBeVisible();
  await second.close();
});

test('waiting_list_names_the_settings_proposal', async ({ page }) => {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const since = new Date(Date.now() - 12 * 60_000).toISOString();
  seedConversation(api, {
    title: 'Ожидающая настройка',
    activity: {
      state: 'waiting_for_user',
      evidence: { request_kind: 'settings_change', question: 'Ждёт решения по настройке: routing' },
      since
    }
  });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });

  const lane = page.locator('[data-section-slot="waiting"]');
  await expect(lane).toBeVisible();
  await expect(lane).toContainText('Ожидающая настройка');
  await expect(lane).toContainText('Ждёт решения по настройке');
});

test('long_preset_diff_scrolls_inside_the_card_without_overflowing_the_column', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  const before = Array.from({ length: 120 }, (_, n) => `строка ${n}`).join('\n');
  const after = Array.from({ length: 160 }, (_, n) => `строка ${n}`).join('\n');
  open(hub, id, api.id(), 'settings_change', { ...PRESET, before, after });

  const card = page.locator('[data-setting="preset"]');
  const diff = card.locator('.diff');
  await expect(diff).toBeVisible();
  expect(await diff.evaluate((node) => node.scrollHeight > node.clientHeight + 1)).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
    page.viewportSize()!.width
  );
  await diff.evaluate((node) => node.scrollTo({ top: node.scrollHeight }));
  expect(await diff.evaluate((node) => node.scrollTop)).toBeGreaterThan(0);
});

test('card_follows_the_visual_language_without_emoji_or_check_marks', async ({ page }) => {
  const { hub, api, id } = await bootChat(page);
  open(hub, id, api.id(), 'settings_change', ROUTING);
  open(hub, id, api.id(), 'settings_change', PRESET);
  const card = page.locator('[data-setting="routing"]');
  const preset = page.locator('[data-setting="preset"]');
  await expect(card).toBeVisible();
  await expect(preset).toBeVisible();

  const colour = (token: string) =>
    page.evaluate((name) => {
      const probe = document.createElement('span');
      probe.style.color = `var(${name})`;
      document.body.append(probe);
      const value = getComputedStyle(probe).color;
      probe.remove();
      return value;
    }, token);
  const added = await card.locator('[data-mark="added"]').first().evaluate((node) => getComputedStyle(node).color);
  const removed = await card.locator('[data-mark="removed"]').first().evaluate((node) => getComputedStyle(node).color);
  const changed = await card.locator('[data-mark="changed"]').first().evaluate((node) => getComputedStyle(node).color);
  expect(added).toBe(await colour('--success'));
  expect(removed).toBe(await colour('--danger'));
  expect(changed).toBe(await colour('--warning'));

  const text = (await card.innerText()) + (await preset.innerText());
  for (const glyph of ['✓', '✔', '✅']) expect(text).not.toContain(glyph);
  expect(Array.from(text).some((char) => (char.codePointAt(0) ?? 0) > 0x1f000)).toBe(false);
  await expect(preset.locator('[data-mark="added"]')).toContainText('+ третья строка');
  await expect(preset.locator('[data-mark="removed"]')).toContainText('− вторая строка');

  const allow = card.locator('button[data-action="allow"]');
  expect((await allow.boundingBox())!.height).toBeGreaterThanOrEqual(36);
});
