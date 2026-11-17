import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const DAY = 86_400_000;

/** Дата в стиле подсказок: «12.12». */
function dayLabel(ts: number): string {
  return new Intl.DateTimeFormat('ru-RU', { day: '2-digit', month: '2-digit' }).format(ts);
}

async function boot(page: Page, expiresAt: string | null) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  const ids = seedMockupSessions(api);
  const claude = api.limits.find((row) => row.provider === 'claude')!;
  claude.credential_expires_at = expiresAt;
  await installChatRoutes(page, hub, api, OWNER);
  return { hub, api, ids };
}

async function openStrip(page: Page) {
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  const strip = page.locator('.limits').first();
  await expect(strip).toBeVisible();
  if (!(await strip.evaluate((node) => (node as HTMLDetailsElement).open))) await strip.locator('summary').click();
  return strip;
}

async function tokenColor(page: Page, strip: ReturnType<Page['locator']>, token: string): Promise<string> {
  return strip.evaluate((node, name) => {
    const probe = document.createElement('span');
    probe.style.color = `var(${name})`;
    node.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, token);
}

const STATES = [
  {
    name: 'far',
    offset: 60 * DAY,
    tone: 'normal',
    text: (ts: number) => `Вход Claude до ${dayLabel(ts)}`
  },
  {
    name: 'within_seven_days',
    offset: 3 * DAY,
    tone: 'warning',
    text: (ts: number) => `Вход Claude заканчивается ${dayLabel(ts)} — войдите заново`
  },
  {
    name: 'past',
    offset: -1 * DAY,
    tone: 'danger',
    text: () => 'Вход Claude, вероятно, закончился — войдите заново'
  }
] as const;

for (const state of STATES) {
  test(`credential_login_${state.name}_is_named_on_the_limits_strip`, async ({ page }) => {
    const at = Date.now() + state.offset;
    await boot(page, new Date(at).toISOString());
    const strip = await openStrip(page);
    const line = strip.locator('.credential[data-provider="claude"]');
    await expect(line).toHaveText(state.text(at));
    await expect(line).toHaveAttribute('data-tone', state.tone);
    if (state.tone === 'warning') {
      expect(await line.evaluate((node) => getComputedStyle(node).color)).toBe(await tokenColor(page, strip, '--warning'));
    }
    if (state.tone === 'danger') {
      expect(await line.evaluate((node) => getComputedStyle(node).color)).toBe(await tokenColor(page, strip, '--danger'));
    }
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });
}

test('credential_login_date_is_named_on_the_machine_card', async ({ page }) => {
  const at = Date.now() + 3 * DAY;
  // Карточка машины читает свою дату из своего же поля, а не из общей полосы.
  await bootSecondary(page, '/machines', (api) => {
    const machine = [...api.machines.values()][0]!;
    machine.credentials = [{ provider: 'claude', expires_at: new Date(at).toISOString(), observed_at: new Date().toISOString() }];
  });
  const line = page.locator('main .credential[data-provider="claude"]');
  await expect(line).toHaveText(`Вход Claude заканчивается ${dayLabel(at)} — войдите заново`);
  await expect(line).toHaveAttribute('data-tone', 'warning');
});
