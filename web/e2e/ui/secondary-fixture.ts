import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes, seedSecondaryScreens } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect } from './fixtures';

export async function bootSecondary(page: Page, path = '/machines', logout: string | null = null, patch?: (api: FakeHubApi) => void, hubVersion?: string) {
  const hub = new FakeHub(); const api = new FakeHubApi(hub); const ids = seedMockupSessions(api);
  patch?.(api);
  await installChatRoutes(page, hub, api, ids.pinned);
  const fixture = await seedSecondaryScreens(page, api, ids, logout, hubVersion);
  await page.goto(path);
  await expect(page.locator('[data-layout]')).toBeVisible();
  return { hub, api, ids, ...fixture };
}
