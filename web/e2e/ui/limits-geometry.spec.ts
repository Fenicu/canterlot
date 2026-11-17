import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import type { Page } from '@playwright/test';
import { FakeHub, FakeHubApi, seedConversation } from '../../testing/fake-hub';
import { installChatRoutes } from '../../testing/fake-hub/http-playwright';
import { seedMockupSessions } from '../../testing/fixtures/mockup-sessions';
import { expect, resizeViewport, test } from './fixtures';
import { bootRealisticLimits } from './limits-seed';

const OWNER = '00000000-0000-4000-8000-0000000000d1';
const SHOTS = process.env.LIMITS_SHOTS_DIR;
const phone = () => test.info().project.name === 'phone';

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  mkdirSync(SHOTS, { recursive: true });
  const theme = await page.locator('html').getAttribute('data-theme');
  await page.screenshot({ path: join(SHOTS, `${name}-${test.info().project.name}-${theme ?? 'dark'}.png`) });
}

interface Box {
  l: number;
  t: number;
  r: number;
  b: number;
  w: number;
  h: number;
}

/** Пересечение прямоугольников заметнее половины пикселя: касание краями не считается. */
function intersects(a: Box, b: Box): boolean {
  return Math.min(a.r, b.r) - Math.max(a.l, b.l) > 0.5 && Math.min(a.b, b.b) - Math.max(a.t, b.t) > 0.5;
}

const observed = () => new Date(Date.now() - 60_000).toISOString();
const win = (used: number, hours: number) => ({
  used_permille: used,
  resets_at: new Date(Date.now() + hours * 3_600_000).toISOString(),
  observed_at: observed()
});

/** Провайдеры как в жизни: у Claude с «резервом» текст плашки переносится на две строки, у Codex — нет. */
function providers(api: FakeHubApi, count: number, reserve: boolean): void {
  const all = [
    {
      provider: 'claude',
      weekly_reserve_active: reserve,
      weekly_reserve_resets_at: reserve ? new Date(Date.now() + 2 * 86_400_000).toISOString() : null,
      five_hour: win(610, 4.97),
      weekly: win(955, 47.5)
    },
    { provider: 'codex', weekly_reserve_active: false, five_hour_absent: true, weekly: win(420, 96) },
    {
      provider: 'opencode',
      weekly_reserve_active: false,
      five_hour: win(120, 3.2),
      weekly: win(300, 120),
      monthly: win(410, 500)
    },
    { provider: 'gemini', weekly_reserve_active: false, five_hour: win(0, 1), weekly: win(0, 2) }
  ];
  api.limits = all.slice(0, count) as never;
}

async function bootList(page: Page, patch?: (api: FakeHubApi) => void) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  patch?.(api);
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto('/sessions');
  await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
  return { hub, api };
}

async function bootChat(page: Page) {
  const hub = new FakeHub();
  const api = new FakeHubApi(hub);
  seedMockupSessions(api);
  const id = seedConversation(api, { title: 'Лимиты в шапке', harness: { id: 'claude', title: 'Claude', provider: 'claude' } });
  await installChatRoutes(page, hub, api, OWNER);
  await page.goto(`/c/${id}`);
  await expect(page.locator('[data-ready]')).toBeVisible({ timeout: 15_000 });
  return page.locator('.chat-header .limits');
}

/** Плашки полосы лимитов и их геометрия. */
async function plaques(page: Page) {
  const strip = page.locator('.limits').first();
  await expect(strip.locator('[data-limit-plaques]')).toBeVisible();
  return strip.evaluate((root) => {
    const round = (n: number) => Math.round(n * 10) / 10;
    return [...root.querySelectorAll<HTMLElement>('[data-limit-plaques] .plaque')].map((plaque) => {
      const rect = (node: Element | null) => {
        if (!node || !node.getClientRects().length) return null;
        const r = node.getBoundingClientRect();
        return { l: round(r.left), t: round(r.top), r: round(r.right), b: round(r.bottom), w: round(r.width), h: round(r.height) };
      };
      const text = plaque.querySelector<HTMLElement>('.plaque-text')!;
      const bar = plaque.querySelector<HTMLElement>('.bar');
      const meter = plaque.querySelector<HTMLElement>('[role="meter"]');
      const clipped = [...text.querySelectorAll<HTMLElement>('*')]
        .filter((node) => !node.classList.contains('sr-only') && node.clientWidth > 0 && node.scrollWidth > node.clientWidth + 0.5)
        .map((node) => ({ text: (node.textContent ?? '').trim(), overflow: getComputedStyle(node).textOverflow }));
      return {
        provider: plaque.dataset.provider ?? '',
        box: rect(plaque)!,
        text: rect(text)!,
        bar: rect(bar),
        barDisplay: bar ? getComputedStyle(bar).display : 'absent',
        gauge: rect(meter),
        textOverflow: text.scrollWidth - text.clientWidth,
        clipped
      };
    });
  });
}

test('f11_the_compact_header_gauge_has_as_much_air_on_its_right_as_on_its_left', async ({ page }) => {
  const strip = await bootChat(page);
  const widths = phone() ? [360, 390, 412] : [1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: phone() ? 892 : 900 });
    await shot(page, `f11-header-limits-${width}`);
    const gaps = await strip.evaluate((root) => {
      const round = (n: number) => Math.round(n * 10) / 10;
      const ink = (node: Element) => {
        const range = document.createRange();
        range.selectNodeContents(node);
        const rect = range.getBoundingClientRect();
        return { l: round(rect.left), r: round(rect.right) };
      };
      return [...root.querySelectorAll<HTMLElement>('.meter')]
        .filter((meter) => meter.querySelector('.bar') && meter.querySelector('.pct'))
        .map((meter) => {
          const bar = meter.querySelector<HTMLElement>('.bar')!.getBoundingClientRect();
          const tag = meter.querySelector<HTMLElement>('.tag')!;
          const unit = meter.querySelector<HTMLElement>('.pct .unit')!;
          return {
            window: meter.dataset.window ?? '',
            labelGap: round(bar.left - ink(tag).r),
            valueGap: round(ink(unit).l - bar.right)
          };
        });
    });
    expect(gaps.length).toBeGreaterThanOrEqual(2);
    for (const gap of gaps) {
      expect(gap.valueGap, `${width}px ${gap.window}: gauge to «ост.»`).toBeGreaterThanOrEqual(4);
      expect(Math.abs(gap.valueGap - gap.labelGap), `${width}px ${gap.window}: gauge sits between label and value`).toBeLessThanOrEqual(0.5);
    }
  }
});

for (const variant of [
  { count: 2, reserve: true },
  { count: 3, reserve: true },
  { count: 3, reserve: false }
] as const) {
  test(`f12_plaque_gauges_share_the_bottom_edge_with_${variant.count}_providers${variant.reserve ? '_and_reserve' : ''}`, async ({ page }) => {
    test.skip(!phone(), 'плашки телефона');
    await bootList(page, (api) => providers(api, variant.count, variant.reserve));
    for (const width of [360, 390, 412]) {
      await resizeViewport(page, { width, height: 892 });
      await shot(page, `f12-plaques-${width}-${variant.count}p${variant.reserve ? '-reserve' : ''}`);
      const items = await plaques(page);
      expect(items).toHaveLength(variant.count);
      for (const item of items) expect(item.bar, `${width}px ${item.provider} gauge`).not.toBeNull();
      const bottoms = items.map((item) => item.bar!.b);
      for (const bottom of bottoms) {
        expect(Math.abs(bottom - bottoms[0]!), `${width}px gauge bottoms: ${bottoms.join(' / ')}`).toBeLessThanOrEqual(0.5);
      }
    }
  });
}

test('f19_desktop_plaques_keep_their_gauge_and_lay_out_two_per_row_when_crowded', async ({ page }) => {
  test.skip(phone(), 'боковая колонка рабочего стола');
  for (const count of [2, 4]) {
    await bootList(page, (api) => providers(api, count, count === 4));
    for (const width of [1024, 1440]) {
      await resizeViewport(page, { width, height: width === 1024 ? 768 : 900 });
      await shot(page, `f19-sidebar-${width}-${count}providers`);
      const items = await plaques(page);
      expect(items).toHaveLength(count);
      // Полоска плашки видна в обеих темах: контейнерное правило для свёрнутых строк её больше не прячет.
      for (const item of items) {
        expect(item.bar, `${width}px ${item.provider}: gauge is drawn`).not.toBeNull();
        expect(item.gauge!.h, `${width}px ${item.provider}: gauge has height`).toBeGreaterThan(0);
      }
      // Никакой текст не обрезан без многоточия.
      for (const item of items) {
        expect(item.textOverflow, `${width}px ${item.provider}: plaque text fits`).toBeLessThanOrEqual(0.5);
        for (const clip of item.clipped) expect(clip.overflow, `${width}px ${item.provider} «${clip.text}»`).toBe('ellipsis');
      }
      const tops = [...new Set(items.map((item) => Math.round(item.box.t)))];
      if (count <= 2) expect(tops, `${width}px two providers stay on one row`).toHaveLength(1);
      else {
        // Три и четыре провайдера — не четырёхстрочная колонка, а ряд по две плашки.
        expect(tops.length, `${width}px ${count} providers wrap`).toBeLessThanOrEqual(2);
        expect(Math.min(...items.map((item) => item.box.w)), `${width}px plaques stay readable`).toBeGreaterThan(100);
      }
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(overflow, `${width}px page`).toBeLessThanOrEqual(0);
    }
    // Светлая тема проверяет те же правила: раскладка не зависит от палитры.
    await page.evaluate(() => localStorage.setItem('canterlot.theme', 'light'));
    await page.reload();
    await expect(page.locator('[data-list-ready]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
    await resizeViewport(page, { width: 1440, height: 900 });
    await shot(page, `f19-sidebar-1440-${count}providers-light`);
    const items = await plaques(page);
    expect(items).toHaveLength(count);
    for (const item of items) {
      expect(item.bar, `light ${item.provider}: gauge is drawn`).not.toBeNull();
      expect(item.textOverflow, `light ${item.provider}: plaque text fits`).toBeLessThanOrEqual(0.5);
    }
    // Следующий провайдер снова начинается с тёмной темы.
    await page.evaluate(() => localStorage.setItem('canterlot.theme', 'dark'));
  }
});

test('f19_phone_plaques_stay_one_row_per_provider', async ({ page }) => {
  test.skip(!phone(), 'плашки телефона');
  await bootList(page, (api) => providers(api, 4, true));
  for (const width of [360, 390, 412]) {
    await resizeViewport(page, { width, height: 892 });
    const items = await plaques(page);
    expect(new Set(items.map((item) => Math.round(item.box.t))).size, `${width}px one row`).toBe(1);
    for (const item of items) expect(item.bar, `${width}px ${item.provider}: gauge`).not.toBeNull();
  }
});

/** Линии порогов, подписи значений и дни недели одного графика. */
async function chartGeometry(page: Page) {
  await expect(page.locator('main .charts .chart').first()).toBeVisible();
  return page.locator('main .charts').evaluate((root) => {
    const round = (n: number) => Math.round(n * 10) / 10;
    const rect = (node: Element) => {
      const r = node.getBoundingClientRect();
      return { l: round(r.left), t: round(r.top), r: round(r.right), b: round(r.bottom), w: round(r.width), h: round(r.height), text: (node.textContent ?? '').trim() };
    };
    return [...root.querySelectorAll<HTMLElement>('.chart')].map((chart) => ({
      width: round(chart.getBoundingClientRect().width),
      labels: [...chart.querySelectorAll<HTMLElement>('.num')].map(rect),
      thresholds: [...chart.querySelectorAll<HTMLElement>('.threshold')].map(rect),
      days: [...chart.querySelectorAll<HTMLElement>('.day')].map((day) => ({ ...rect(day), overflow: day.scrollWidth - day.clientWidth }))
    }));
  });
}

test('f15_value_labels_clear_the_threshold_lines_and_each_other_at_every_width', async ({ page }) => {
  await bootRealisticLimits(page);
  const widths = [360, 412, 1024, 1440];
  for (const width of widths) {
    await resizeViewport(page, { width, height: width >= 1024 ? 768 : 892 });
    await page.locator('main .charts').scrollIntoViewIfNeeded();
    await shot(page, `f15-history-${width}-dark`);
    const charts = await chartGeometry(page);
    expect(charts.length).toBe(3);
    for (const chart of charts) {
      expect(chart.labels.length).toBeGreaterThanOrEqual(5);
      for (const label of chart.labels) {
        for (const line of chart.thresholds) {
          expect(intersects(label, line), `${width}px «${label.text}» crossed by a threshold line`).toBe(false);
        }
      }
      for (let index = 1; index < chart.labels.length; index++) {
        expect(intersects(chart.labels[index - 1]!, chart.labels[index]!), `${width}px «${chart.labels[index - 1]!.text}» / «${chart.labels[index]!.text}» overlap`).toBe(false);
      }
      for (const day of chart.days) {
        expect(day.overflow, `${width}px day «${day.text}» is cut`).toBeLessThanOrEqual(0.5);
      }
    }
  }
  // Светлая тема и палитра: метки остаются на тех же местах, меняются только токены.
  await page.getByRole('button', { name: 'Сменить тему' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.locator('main .charts').scrollIntoViewIfNeeded();
  await shot(page, 'f15-history-1440-light');
  for (const chart of await chartGeometry(page)) {
    for (const label of chart.labels) {
      for (const line of chart.thresholds) expect(intersects(label, line), `light «${label.text}»`).toBe(false);
    }
  }
});

test('f15_desktop_1024_gives_each_chart_room_for_labels_and_days', async ({ page }) => {
  await bootRealisticLimits(page);
  await resizeViewport(page, { width: 1024, height: 768 });
  const charts = await chartGeometry(page);
  // В 636 px панели три графика по 206 px режут дни; двух колонок хватает и подписям, и дням.
  for (const chart of charts) {
    expect(chart.width, 'chart keeps a readable column').toBeGreaterThanOrEqual(280);
    for (const day of chart.days) expect(day.overflow, `day «${day.text}»`).toBeLessThanOrEqual(0.5);
  }
});
