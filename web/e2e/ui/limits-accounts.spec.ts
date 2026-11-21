import type { Page } from '@playwright/test';
import { LONG_ACCOUNT, routeAccountHistory, seedSecondOpencodeAccount } from '../../testing/fake-hub/limit-accounts';
import { expect, resizeViewport, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const phone = () => test.info().project.name === 'phone';

async function bootStrip(page: Page, account?: string) {
  const boot = await bootSecondary(page, '/sessions', (api) => seedSecondOpencodeAccount(api, account ? { account } : {}));
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return boot;
}

async function openStrip(page: Page) {
  const strip = page.locator('.limits').first();
  await expect(strip).toBeVisible();
  if (!(await strip.evaluate((node) => (node as HTMLDetailsElement).open))) await strip.locator('summary').click();
  await expect(strip.locator('.rows')).toBeVisible();
  return strip;
}

async function pageOverflow(page: Page): Promise<number> {
  return page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
}

/** Текст вылезает за край своей полосы или карточки: проверяем каждый видимый узел внутри `holder`. */
async function outsideOf(page: Page, holder: string): Promise<string[]> {
  return page.locator(holder).first().evaluate((box) => {
    const outer = box.getBoundingClientRect();
    const out: string[] = [];
    for (const el of box.querySelectorAll<HTMLElement>('*')) {
      if (el.closest('.sr-only') || el.parentElement?.closest('[role="meter"]')) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) continue;
      if (rect.left < outer.left - 0.5 || rect.right > outer.right + 0.5) {
        out.push(`${el.tagName.toLowerCase()}.${String(el.className).split(' ')[0]} «${(el.textContent ?? '').trim().slice(0, 24)}»`);
      }
    }
    return out;
  });
}

test('two_accounts_of_one_provider_are_labelled_on_the_collapsed_and_open_strip', async ({ page }) => {
  await bootStrip(page);
  const plaques = page.locator('[data-limit-plaques] .plaque');
  await expect(plaques.locator('.pname')).toHaveText(['Claude', 'Codex', 'OpenCode · owner', 'OpenCode · b-opencode']);
  await expect(plaques.filter({ has: page.locator('.pname', { hasText: 'Claude' }) })).not.toHaveAttribute('data-account', /.+/);
  await expect(page.locator('[data-limit-plaques] .plaque[data-account="b-opencode"]')).toHaveCount(1);
  const strip = await openStrip(page);
  await expect(strip.locator('.row')).toHaveCount(4);
  await expect(strip.locator('.row[data-account]')).toHaveCount(2);
  await expect(strip.locator('.row[data-account="owner"] .pname').first()).toHaveText('OpenCode · owner');
  await expect(strip.locator('.row[data-account="b-opencode"] .pname').first()).toHaveText('OpenCode · b-opencode');
  // Недели двух записей разные: 66% и 28% остатка, а не одно общее число.
  await expect(strip.locator('.row[data-account="owner"] .meter[data-window="week"] .pct')).toContainText('66%');
  await expect(strip.locator('.row[data-account="b-opencode"] .meter[data-window="week"] .pct')).toContainText('28%');
  // Вход у каждой записи свой: подпись называет запись, а не только провайдера.
  await expect(strip.locator('.credential[data-account="b-opencode"]')).toHaveText(/^Вход OpenCode · b-opencode до /);
  await expect(strip.locator('.credential[data-account="owner"]')).toHaveText(/^Вход OpenCode · owner до /);
  expect(await pageOverflow(page)).toBeLessThanOrEqual(0);
});

test('a_live_update_of_one_account_changes_only_its_row_and_removal_drops_only_its_row', async ({ page }) => {
  const { hub } = await bootStrip(page);
  const strip = await openStrip(page);
  hub.append('conversations', {
    kind: 'limits_changed',
    payload: { provider: 'opencode', account: 'b-opencode', window: 'weekly', used_permille: 900, resets_at: new Date(Date.now() + 86_400_000).toISOString(), observed_at: new Date().toISOString(), weekly_reserve_active: false, weekly_reserve_resets_at: null }
  });
  hub.flush();
  await expect(strip.locator('.row[data-account="b-opencode"] .meter[data-window="week"] .pct')).toContainText('10%');
  await expect(strip.locator('.row[data-account="owner"] .meter[data-window="week"] .pct')).toContainText('66%');
  await expect(strip.locator('.row')).toHaveCount(4);
  hub.append('conversations', {
    kind: 'limits_providers_changed',
    payload: { providers: ['claude', 'codex', 'opencode'], accounts: [{ provider: 'claude', account: 'owner' }, { provider: 'codex', account: 'owner' }, { provider: 'opencode', account: 'owner' }] }
  });
  hub.flush();
  await expect(strip.locator('.row')).toHaveCount(3);
  // С одной записью подпись исчезает: строка выглядит, как всегда.
  await expect(strip.locator('.row .pname').filter({ hasText: 'OpenCode' })).toHaveText(['OpenCode']);
  await expect(strip.locator('.row[data-account]')).toHaveCount(0);
});

test('a_long_account_label_does_not_overflow_the_strip', async ({ page }) => {
  await bootStrip(page, LONG_ACCOUNT);
  const widths = phone() ? [320, 360, 390] : [1280, 1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: phone() ? 844 : 900 });
    expect(await pageOverflow(page), `${width}px collapsed`).toBeLessThanOrEqual(0);
    expect(await outsideOf(page, '.limits'), `${width}px collapsed`).toEqual([]);
    const strip = await openStrip(page);
    await expect(strip.locator(`.row[data-account="${LONG_ACCOUNT}"]`)).toHaveCount(1);
    expect(await pageOverflow(page), `${width}px open`).toBeLessThanOrEqual(0);
    expect(await outsideOf(page, '.limits'), `${width}px open`).toEqual([]);
    await strip.locator('summary').click();
  }
});

async function bootScreen(page: Page, account?: string) {
  const boot = await bootSecondary(page, '/limits', (api) => seedSecondOpencodeAccount(api, account ? { account } : {}));
  await routeAccountHistory(page, boot.ids.pinned, account);
  await page.reload();
  await page.locator('main[data-secondary-ready]').waitFor();
  return boot;
}

test('the_limits_screen_shows_a_card_a_chart_and_a_usage_group_per_account', async ({ page }) => {
  await bootScreen(page);
  const cards = page.locator('main .current .provider');
  await expect(cards.locator('h3')).toHaveText(['Claude', 'Codex', 'OpenCode · owner', 'OpenCode · b-opencode']);
  await expect(cards.filter({ has: page.locator('h3', { hasText: 'Claude' }) })).not.toHaveAttribute('data-account', /.+/);
  const other = page.locator('main .current .provider[data-account="b-opencode"]');
  await expect(other.locator('.row[data-window="week"] .value')).toHaveText('осталось 28%');
  await expect(page.locator('main .current .provider[data-account="owner"] .row[data-window="week"] .value')).toHaveText('осталось 66%');
  await expect(other.locator('.credential')).toHaveText(/^Вход OpenCode · b-opencode до /);
  // Неделя каждой записи — свой график со своей подписью.
  await expect(page.locator('main .charts .chart[data-account="b-opencode"] .name')).toHaveText('OpenCode · b-opencode');
  await expect(page.locator('main .charts .chart[data-account="owner"] .name')).toHaveText('OpenCode · owner');
  // Расход за сегодня считается по записи: у каждой группы свой итог.
  await expect(page.locator('main tbody[data-account="owner"] .total')).toContainText('2,5');
  await expect(page.locator('main tbody[data-account="b-opencode"] .total')).toContainText('6,1');
  expect(await pageOverflow(page)).toBeLessThanOrEqual(0);
});

test('a_long_account_label_does_not_overflow_the_limits_screen', async ({ page }) => {
  await bootScreen(page, LONG_ACCOUNT);
  const widths = phone() ? [320, 360, 390] : [1280, 1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: phone() ? 844 : 900 });
    await expect(page.locator(`main .current .provider[data-account="${LONG_ACCOUNT}"]`)).toHaveCount(1);
    expect(await pageOverflow(page), `${width}px`).toBeLessThanOrEqual(0);
    for (const holder of ['main .current .provider[data-account]', 'main .charts .chart[data-account]', 'main tbody[data-account]']) {
      for (const index of [0, 1]) {
        const node = page.locator(holder).nth(index);
        const box = await node.boundingBox();
        const pane = await page.locator('main .secondary-content').boundingBox();
        expect(box!.x + box!.width, `${holder} ${width}px`).toBeLessThanOrEqual(pane!.x + pane!.width + 0.5);
      }
    }
  }
});
