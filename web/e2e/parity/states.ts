import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedMachine } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockup04, seedMockup05, seedMockup06, seedMockup08, seedMockup11, seedMockupChat } from '../../testing/fixtures/mockup-chat';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect } from '../ui/fixtures';
import { bootSecondary } from '../ui/secondary-fixture';

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
      permission_modes: ['default', 'ask', 'auto_edit', 'full_access']
    }
  ]
};

function fresh() {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMachine(api, WORKSTATION);
  return { hub, api };
}

async function chat(page: Page, seeder: (api: FakeHubApi) => string, afterOpen?: (page: Page) => Promise<void>): Promise<void> {
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
    detail.permission_mode = detail.permission_mode || 'default';
  }
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]').first()).toBeVisible({ timeout: 15_000 });
  await afterOpen?.(page);
}

async function sessions(page: Page, route: string, ready: string): Promise<void> {
  const { hub, api } = fresh();
  seedMockupSessions(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(route);
  await expect(page.locator(ready)).toBeVisible({ timeout: 15_000 });
}

const screen = (name: string, mockup: boolean, viewport: Screen['viewport'], theme: Theme, boot: Screen['boot']): Screen => ({ name, mockup, viewport, theme, boot });

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
  screen('11-desktop', true, DESKTOP, 'dark', async (page) => {
    const { hub, api } = fresh();
    const { id } = seedMockup11(api);
    await installChatRoutes(page, hub, api);
    await page.goto(`/c/${id}`);
    await expect(page.locator('[data-layout="desktop"]')).toBeVisible({ timeout: 15_000 });
    await page.getByRole('button', { name: 'Поручения' }).click();
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
  })
];

export async function bootScreen(page: Page, target: Screen, theme: Theme = target.theme, viewport = target.viewport): Promise<void> {
  await page.setViewportSize(viewport);
  await page.addInitScript((value) => localStorage.setItem('canterlot.theme', value), theme);
  await target.boot(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}
