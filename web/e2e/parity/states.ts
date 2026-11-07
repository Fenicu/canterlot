import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup04, seedMockup05, seedMockup06, seedMockup08, seedMockup11, seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect } from '../ui/fixtures';
import { bootSecondary } from '../ui/secondary-fixture';
import { seedConversation } from '../../testing/fake-hub/seed';
import { DEEPSEEK, seedQueue, seedRoutedSpawn, seedRoutingCatalog, seedRoutingWhitelist } from '../../testing/fixtures/routing';
import { openChat, settled, thumb, viewer } from '../ui/image-viewer-fixture';

export type Theme = 'dark' | 'light';

export interface Screen {
  /** Имя экрана; для первых одиннадцати совпадает с именем PNG утверждённого макета. */
  name: string;
  mockup: boolean;
  viewport: { width: number; height: number };
  theme: Theme;
  /** Маршрут и состояние, которые пользователь видит на экране. */
  boot(page: Page): Promise<void>;
}

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const PHONE = { width: 412, height: 892 };
const DESKTOP = { width: 1440, height: 900 };

const WORKSTATION = {
  id: 'workstation',
  name: 'workstation',
  harnesses: [
    {
      id: 'claude',
      title: 'Claude Code',
      provider: 'claude',
      models: [
        { id: 'opus-5.5', title: 'opus-5.5', efforts: ['low', 'medium', 'high'] },
        { id: 'sonnet-5', title: 'sonnet-5', efforts: ['low', 'medium'] },
        { id: 'haiku', title: 'haiku', efforts: ['low'] }
      ],
      permission_modes: ['owner', 'ask', 'bypass', 'read_only']
    }
  ]
};

function fresh() {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, WORKSTATION);
  return { hub, api };
}

async function chat(page: Page, seeder: (api: FakeHubApi) => string, afterOpen?: (page: Page, hub: FakeHub, api: FakeHubApi, id: string) => Promise<void>): Promise<void> {
  const { hub, api } = fresh();
  const id = seeder(api);
  const detail = api.details.get(id);
  if (detail && !detail.harness) {
    detail.harness = { id: 'claude', title: 'Claude Code', provider: 'claude' };
    detail.machine = { id: 'workstation', name: 'workstation' };
    detail.model = 'opus-5.5';
  }
  if (detail) {
    detail.effort = detail.effort || 'medium';
    detail.permission_mode = detail.permission_mode || 'owner';
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  await afterOpen?.(page, hub, api, id);
}

async function menuItem(page: Page, item: string): Promise<void> {
  await page.getByRole('button', { name: 'Меню сессии' }).click();
  await page.getByRole('menuitem', { name: item }).click();
}

function openRequest(hub: FakeHub, api: FakeHubApi, id: string, kind: string, payload: unknown): void {
  hub.append(`conversation:${id}`, { kind: 'request_opened', conversation_id: id, payload: { request_id: api.id(), kind, payload } });
  hub.flush();
}

async function sessions(page: Page, route: string, ready: string): Promise<void> {
  const { hub, api } = fresh();
  seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(route);
  await expect(page.locator(ready)).toBeVisible({ timeout: 15_000 });
}

/** Редактор маршрутизации с подсветкой пары вне белого списка и раскрытым кандидатом. */
async function routingEditor(page: Page, viewport = PHONE): Promise<{ api: FakeHubApi }> {
  const { hub, api } = fresh();
  seedRoutingCatalog(api);
  seedRoutingWhitelist(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.setViewportSize(viewport);
  await page.goto('/settings/routing');
  await expect(page.locator('[data-routing-editor][data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  return { api };
}

/** Оркестратор с лентой запусков, очередью и делегатом, выбранным маршрутом. */
async function orchestrator(page: Page, afterOpen?: (page: Page, hub: FakeHub, api: FakeHubApi, id: string, child: string) => Promise<void>, open?: 'child'): Promise<void> {
  const { hub, api } = fresh();
  seedRoutingCatalog(api);
  const id = seedConversation(api, { title: 'Оркестратор', harness: { id: 'claude_code', title: 'Claude Code', provider: 'claude' } });
  const child = seedRoutedSpawn(api, id);
  seedQueue(api, id);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${open === 'child' ? child : id}`);
  await expect(page.locator('.screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  await afterOpen?.(page, hub, api, id, child);
}

const screen = (name: string, mockup: boolean, viewport: Screen['viewport'], theme: Theme, boot: Screen['boot']): Screen => ({ name, mockup, viewport, theme, boot });

/** Просмотр картинки из присланного файла. Фокус на «Закрыть» держит кнопки на экране, иначе они спрячутся до проверки. */
async function imageViewer(page: Page): Promise<void> {
  await openChat(page, 'shared');
  await thumb(page, 'shared', 0).click();
  await expect(viewer(page).locator('.slide[data-current] .frame[data-ready]')).toBeVisible();
  await settled(page);
  await page.keyboard.press('Tab');
  await expect(viewer(page).getByRole('button', { name: 'Закрыть' })).toBeFocused();
}

export const SCREENS: Screen[] = [
  screen('01-sessions', true, PHONE, 'dark', (page) => sessions(page, '/sessions', '[data-list-ready]')),
  screen('02-chat', true, PHONE, 'dark', (page) => chat(page, seedMockupChat)),
  screen('03-chat-light', true, PHONE, 'light', (page) => chat(page, seedMockupChat)),
  screen('04-info-sheet', true, PHONE, 'dark', (page) => chat(page, (api) => seedMockup04(api).id, async (opened) => {
    await opened.getByRole('button', { name: 'Сведения о ходе' }).first().click();
    await expect(opened.getByRole('dialog', { name: 'Об ответе' })).toBeVisible();
  })),
  screen('05-subagent-lane', true, PHONE, 'dark', async (page) => {
    const { hub, api } = fresh();
    const { id, rootBlockId } = seedMockup05(api);
    await installChatRoutes(page, hub, api);
    await page.goto(`/c/${id}/lane/${rootBlockId}`);
    await expect(page.locator('.lane-screen[data-ready]')).toBeVisible({ timeout: 15_000 });
  }),
  screen('06-question', true, PHONE, 'dark', (page) => chat(page, seedMockup06)),
  screen('07-new-session', true, PHONE, 'dark', async (page) => {
    await sessions(page, '/sessions', '[data-list-ready]');
    await page.goto('/new');
    await expect(page.locator('[data-new-session]')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('combobox', { name: 'Машина' })).toBeVisible();
  }),
  screen('08-quota-paused', true, PHONE, 'dark', (page) => chat(page, seedMockup08)),
  screen('09-machine', true, PHONE, 'dark', async (page) => {
    await bootSecondary(page, '/machines');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
  }),
  screen('10-limits-history', true, PHONE, 'dark', async (page) => {
    await bootSecondary(page, '/limits');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
  }),
  // Широкий экран лимитов: карточки, недели и расход стоят рядами, а не колонкой телефона.
  screen('limits-desktop', false, DESKTOP, 'dark', async (page) => {
    await bootSecondary(page, '/limits');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
  }),
  screen('11-desktop', true, DESKTOP, 'dark', async (page) => {
    const { hub, api } = fresh();
    const { id } = seedMockup11(api);
    await installChatRoutes(page, hub, api);
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: 'Делегирование' }).click();
    await expect(page.locator('[data-pane="lane"]')).toBeVisible();
    await page.locator('[data-pane="lane"]').getByText('Explore').first().click();
    await expect(page.locator('[data-pane="lane"] .selected-lane')).toBeVisible();
  }),
  // Экраны без утверждённого макета строятся из тех же компонентов.
  screen('settings', false, PHONE, 'dark', async (page) => {
    await bootSecondary(page, '/settings');
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
  }),
  screen('share', false, PHONE, 'dark', async (page) => {
    await bootSecondary(page, '/share');
    await expect(page.locator('main')).toBeVisible();
  }),
  // Состояния поверх экранов: листы, меню, очередь и карточки, которые не видны при обычном открытии.
  screen('session-sheet', false, PHONE, 'dark', async (page) => {
    await sessions(page, '/sessions', '[data-list-ready]');
    await page.getByRole('button', { name: /^Действия:/ }).first().click();
    await expect(page.getByRole('dialog').first()).toBeVisible();
  }),
  screen('session-popover', false, DESKTOP, 'dark', async (page) => {
    await sessions(page, '/sessions', '[data-list-ready]');
    await page.getByRole('button', { name: /^Действия:/ }).first().click();
    await expect(page.getByRole('dialog').first()).toBeVisible();
  }),
  screen('trash', false, PHONE, 'dark', async (page) => {
    const { hub, api } = fresh();
    const ids = seedMockupSessions(api);
    api.trashConversation(ids.plain);
    await installChatRoutes(page, hub, api, OWNER);
    await page.addInitScript(() => localStorage.setItem('canterlot.list-tab', JSON.stringify({ kind: 'trash' })));
    await page.goto('/sessions');
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: /Корзина/ })).toBeVisible();
  }),
  screen('chat-trashed', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened, _hub, api, id) => {
    api.trashConversation(id);
    await expect(opened.getByRole('heading', { name: 'Сессия в корзине' })).toBeVisible();
  })),
  screen('group-manager', false, PHONE, 'dark', async (page) => {
    await sessions(page, '/sessions', '[data-list-ready]');
    await page.getByRole('button', { name: 'Ещё' }).click();
    await page.getByRole('menuitem', { name: 'Группы' }).click();
    await expect(page.getByRole('dialog', { name: 'Группы' })).toBeVisible();
  }),
  screen('session-menu', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    await opened.getByRole('button', { name: 'Меню сессии' }).click();
    await expect(opened.getByRole('menu')).toBeVisible();
  })),
  screen('settings-sheet', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    await menuItem(opened, 'Настройки сессии');
    await expect(opened.getByRole('dialog', { name: 'Настройки сессии' })).toBeVisible();
  })),
  screen('settings-pane', false, DESKTOP, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    await menuItem(opened, 'Настройки сессии');
    await expect(opened.locator('[data-pane="lane"] #model')).toBeVisible();
  })),
  screen('diagnostics', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    await menuItem(opened, 'Диагностика');
    await expect(opened.getByRole('dialog', { name: 'Диагностика' })).toBeVisible();
  })),
  screen('send-later', false, DESKTOP, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    const field = opened.getByRole('textbox', { name: 'Сообщение' });
    await field.fill('Черновик');
    await field.press('Control+Shift+Enter');
    await expect(opened.getByRole('dialog', { name: 'Отправить позже' })).toBeVisible();
  })),
  screen('queue', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened, hub, api, id) => {
    for (const [index, text] of ['Проверь гонку двух подписок', 'Собери итог', 'Обнови README', 'Закрой задачу'].entries()) {
      hub.append(`conversation:${id}`, { kind: 'input_queued', conversation_id: id, payload: { input_id: api.id(), input_seq: String(index + 50), idempotency_key: api.id(), state: 'queued', content: [{ type: 'text', text }] } });
    }
    hub.flush();
    await expect(opened.getByRole('button', { name: /^Ещё \d+ в очереди/ })).toBeVisible();
  })),
  screen('attachments', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened) => {
    await opened.setInputFiles('input[type=file]', { name: 'hub.log', mimeType: 'text/plain', buffer: Buffer.from('журнал') });
    await expect(opened.locator('.tray')).toBeVisible();
  })),
  screen('markdown-viewer', false, PHONE, 'dark', async (page) => {
    const { hub, api } = fresh();
    const id = seedConversation(api);
    const blockId = api.id();
    api.blocks.set(blockId, { text: '', rev: 1, state: 'final' });
    api.histories.set(id, [{
      kind: 'block',
      id: blockId,
      block_kind: 'tool_call',
      content: '',
      content_bytes: 0,
      content_truncated: false,
      created_seq: '1',
      created_at: '2026-11-05T12:00:00.000Z',
      rev: 1,
      state: 'final',
      agent: 'агент',
      model: 'opus',
      tool_name: 'share_file',
      summary: 'Документы',
      parent_block_id: null,
      input_command_id: null,
      line_count: 0,
      files: [{ attachment_id: 'parity-doc', name: 'report.md', mime: 'text/markdown', size: 1024, title: 'Отчёт', note: '', width: null, height: null }]
    }]);
    api.bindConversation(id);
    await installChatRoutes(page, hub, api, OWNER);
    await page.route('**/api/v1/attachments/*/content', async (route) => {
      await route.fulfill({
        contentType: 'text/markdown; charset=utf-8',
        body: '# Отчёт по сборке\n\nСборка прошла успешно.\n\n| Шаг | Время |\n| --- | --- |\n| Сборка | 12 с |\n| Тесты | 40 с |\n\n- [x] Проверки\n- [ ] Выкладка\n\n```rust\nfn main() {\n    println!("готово");\n}\n```\n\n[Локальный файл](docs/readme.md)'
      });
    });
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: 'Открыть документ report.md' }).click();
    const dialog = page.getByRole('dialog', { name: 'report.md' });
    await expect(dialog).toBeVisible();
    await expect(dialog.locator('h1')).toBeVisible();
  }),
  screen('permission-request', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened, hub, api, id) => {
    openRequest(hub, api, id, 'permission', { tool_name: 'Bash', description: 'Запустить тесты', command: 'cargo test' });
    await expect(opened.locator('.permission-card')).toBeVisible();
  })),
  screen('url-request', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened, hub, api, id) => {
    openRequest(hub, api, id, 'permission', { tool_name: 'mcp_elicitation', server: 'github', mode: 'url', message: 'Подтвердите доступ', url: 'https://github.com/login/device?code=WDJB' });
    await expect(opened.locator('.permission-card')).toBeVisible();
  })),
  screen('routing-editor', false, PHONE, 'dark', async (page) => {
    await routingEditor(page);
    await page.locator('[data-task-key="bugfix"] details.more').first().locator('summary').click();
  }),
  screen('routing-preview', false, PHONE, 'dark', async (page) => {
    const { api } = await routingEditor(page);
    api.routingSkips.set(`opencode/${DEEPSEEK}`, { verdict: 'parallel_busy', reason: `4 of 4 ${DEEPSEEK} sessions busy` });
    api.routingGuard = { stale: false, blocked: true, violations: ['disk 12 GB free < 30 GB'], detail: 'disk 12 GB free < 30 GB.' };
    const preview = page.locator('[data-role="routing-preview"]');
    await preview.getByRole('button', { name: 'Проверить' }).click();
    await expect(preview.locator('[data-role="preview-result"]')).toBeVisible();
    await preview.scrollIntoViewIfNeeded();
  }),
  screen('spawn-queue', false, PHONE, 'dark', (page) => orchestrator(page, async (opened) => {
    await opened.getByRole('button', { name: /В очереди запуска/ }).click();
    await expect(opened.locator('[data-queue-item]')).toHaveCount(2);
  })),
  screen('delegation-queued', false, PHONE, 'dark', (page) => orchestrator(page, async (opened) => {
    await opened.getByRole('button', { name: 'Делегирование' }).click();
    await expect(opened.locator('[data-queue-node]').first()).toBeVisible();
  })),
  screen('route-sheet', false, PHONE, 'dark', (page) => orchestrator(page, async (opened) => {
    await opened.locator('[data-role="route-state-line"]').click();
    await expect(opened.getByRole('dialog', { name: 'Выбор маршрута' })).toBeVisible();
    // Строка в шапке под указателем меняет цвет переходом: проверка смотрит на устоявшийся цвет.
    await opened.waitForFunction(() => document.getAnimations().every((animation) => animation.playState !== 'running'));
  }, 'child')),
  screen('image-viewer', false, PHONE, 'dark', imageViewer),
  screen('image-viewer-desktop', false, DESKTOP, 'light', imageViewer),
  screen('question-request', false, PHONE, 'dark', (page) => chat(page, seedMockupChat, async (opened, hub, api, id) => {
    openRequest(hub, api, id, 'question', { truncated: true, questions: [{ id: 'q0', text: 'Что делать?', options: ['Да', 'Нет'] }, { id: 'q1', text: 'Какие тесты?', options: ['Сон', 'Сеть'], multi_select: true, allow_custom: true }] });
    await expect(opened.locator('.question-card')).toBeVisible();
  }))
];

export async function bootScreen(page: Page, target: Screen, theme: Theme = target.theme, viewport = target.viewport): Promise<void> {
  await page.setViewportSize(viewport);
  await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
  await target.boot(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}
