import type { Page } from '@playwright/test';
import { PALETTES } from '../../src/lib/palettes';
import { test, expect } from './fixtures';
import { bootRealisticLimits } from './limits-seed';

const phone = () => test.info().project.name === 'phone';

/** Элементы содержимого, вылезающие за свой экран или за ближайший контейнер-карточку. */
async function overflowing(page: Page) {
  return page.locator('main .secondary-content').evaluate((content) => {
    const box = content.getBoundingClientRect();
    const out: string[] = [];
    for (const el of content.querySelectorAll<HTMLElement>('*')) {
      // Заливка шкалы сдвинута внутри дорожки с overflow: hidden и за её край не видна.
      if (el.closest('.sr-only') || el.parentElement?.closest('[role="meter"]')) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) continue;
      const holder = el.parentElement?.closest('.provider, .chart, table, .empty') ?? content;
      const outer = holder === content ? box : holder.getBoundingClientRect();
      if (rect.left < outer.left - 0.5 || rect.right > outer.right + 0.5 || rect.right > box.right + 0.5) {
        out.push(`${el.tagName.toLowerCase()}.${String(el.className).split(' ')[0]} «${(el.textContent ?? '').trim().slice(0, 24)}» ${Math.round(rect.left)}..${Math.round(rect.right)} в ${Math.round(outer.left)}..${Math.round(outer.right)}`);
      }
    }
    return out;
  });
}

test('limits_screen_never_overflows_at_phone_and_desktop_widths', async ({ page }) => {
  await bootRealisticLimits(page);
  const widths = phone() ? [320, 360, 390] : [1280, 1440];
  for (const width of widths) {
    await page.setViewportSize({ width, height: phone() ? 844 : 900 });
    await page.waitForTimeout(100);
    expect(await overflowing(page), `${width}px`).toEqual([]);
    const scroll = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(scroll, `${width}px`).toBeLessThanOrEqual(0);
  }
});

test('desktop_layout_uses_the_width_with_regions_side_by_side', async ({ page }) => {
  test.skip(phone(), 'раскладка рабочего стола');
  await bootRealisticLimits(page);
  for (const width of [1280, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await page.waitForTimeout(100);
    const pane = (await page.locator('main').boundingBox())!;
    const content = (await page.locator('main .secondary-content').boundingBox())!;
    // Содержимое занимает почти всю панель и стоит по её центру, а не узкой колонкой у края.
    expect(content.width / pane.width, `${width}px`).toBeGreaterThan(0.85);
    const left = content.x - pane.x;
    const right = pane.x + pane.width - (content.x + content.width);
    expect(Math.abs(left - right), `${width}px`).toBeLessThanOrEqual(2);
    // Карточки провайдеров — один ряд, и неделя каждого стоит ровно под его карточкой.
    const cards = await page.locator('main .current .provider').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().toJSON() as DOMRect));
    const charts = await page.locator('main .charts > *').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().toJSON() as DOMRect));
    expect(cards).toHaveLength(3);
    expect(charts).toHaveLength(3);
    for (let index = 0; index < 3; index++) {
      expect(Math.abs(cards[index]!.y - cards[0]!.y)).toBeLessThanOrEqual(1);
      expect(Math.abs(charts[index]!.x - cards[index]!.x)).toBeLessThanOrEqual(1);
      expect(Math.abs(charts[index]!.width - cards[index]!.width)).toBeLessThanOrEqual(1);
    }
    expect(cards[2]!.x + cards[2]!.width).toBeGreaterThan(content.x + content.width - 2);
    // Расход за сегодня и скорость — рядом, расход шире.
    const usage = (await page.locator('[data-region="usage"]').boundingBox())!;
    const speed = (await page.locator('[data-region="speed"]').boundingBox())!;
    expect(Math.abs(usage.y - speed.y)).toBeLessThanOrEqual(1);
    expect(speed.x).toBeGreaterThan(usage.x + usage.width);
    expect(usage.width).toBeGreaterThan(speed.width * 1.5);
    // Шапка экрана расширяется вместе с содержимым: кнопки не висят посреди панели.
    const header = (await page.locator('main header.topbar').boundingBox())!;
    expect(header.width / content.width).toBeGreaterThan(0.9);
  }
});

test('phone_layout_stacks_regions_and_keeps_every_card_full_width', async ({ page }) => {
  test.skip(!phone(), 'раскладка телефона');
  await page.setViewportSize({ width: 390, height: 844 });
  await bootRealisticLimits(page);
  const content = (await page.locator('main .secondary-content').boundingBox())!;
  for (const box of await page.locator('main .current .provider, main .charts > *').evaluateAll((nodes) => nodes.map((node) => node.getBoundingClientRect().toJSON() as DOMRect))) {
    expect(Math.abs(box.width - content.width)).toBeLessThanOrEqual(1);
  }
  const usage = (await page.locator('[data-region="usage"]').boundingBox())!;
  const speed = (await page.locator('[data-region="speed"]').boundingBox())!;
  expect(speed.y).toBeGreaterThanOrEqual(usage.y + usage.height);
  // Сколько осталось у всех трёх провайдеров видно на первом экране.
  const third = (await page.locator('main .current .provider').nth(2).boundingBox())!;
  expect(third.y + third.height).toBeLessThanOrEqual(844);
});

test('near_exhaustion_and_stale_readings_are_said_in_words_colour_and_gauges', async ({ page }) => {
  await bootRealisticLimits(page);
  const claude = page.getByRole('region', { name: 'Claude, текущие лимиты' });
  // Крупно — самое тесное окно: у Claude это неделя, и она на исходе.
  await expect(claude).toHaveAttribute('data-tone', 'danger');
  await expect(claude.locator('.lead b')).toHaveText('4%');
  await expect(claude.locator('.lead span')).toHaveText('осталось на неделю');
  await expect(claude.locator('.status .word')).toHaveText('на исходе');
  await expect(claude.locator('[data-window="week"] .value')).toHaveText('осталось 4%');
  await expect(claude.locator('[data-window="week"] [role="meter"]')).toHaveAttribute('data-tone', 'danger');
  const danger = await claude.locator('.lead b').evaluate((node) => getComputedStyle(node).color);
  const dangerToken = await page.evaluate(() => { const probe = document.createElement('span'); probe.style.color = 'var(--danger)'; document.body.append(probe); const value = getComputedStyle(probe).color; probe.remove(); return value; });
  expect(danger).toBe(dangerToken);
  // Устаревшее показание не выдаёт себя за текущее: числа нет, есть время последнего показания.
  const codex = page.getByRole('region', { name: 'Codex, текущие лимиты' });
  await expect(codex).toHaveAttribute('data-stale', 'true');
  await expect(codex.locator('.lead b')).toHaveText('устарело');
  await expect(codex.locator('.lead span')).toHaveText(/^показание \S/);
  await expect(codex.locator('.row .value')).toHaveText(['устарело', 'устарело']);
  await expect(codex.locator('.row .reset')).toHaveCount(0);
  // У OpenCode есть месяц, и тесней всего сейчас он.
  const opencode = page.getByRole('region', { name: 'OpenCode, текущие лимиты' });
  await expect(opencode.locator('.row .label')).toHaveText(['5 часов', 'неделя', 'месяц']);
  await expect(opencode.locator('.lead span')).toHaveText('осталось на месяц');
  // На графике последняя точка Claude ниже резерва и красная, пороги подписаны на оси и в описании.
  const chart = page.getByRole('img', { name: /^Недельный остаток по дням\. Claude/ });
  await expect(chart).toHaveAccessibleName(/порог 20%, резерв 5%$/);
  await expect(chart.locator('.dot').last()).toHaveAttribute('data-tone', 'danger');
  await expect(chart.locator('.tick')).toHaveText(['20%', '5%', '100%', '50%']);
  // День без снимка не соединяется линией: у OpenCode два отрезка и шесть точек.
  const opencodeChart = page.getByRole('img', { name: /^Недельный остаток по дням\. OpenCode/ });
  await expect(opencodeChart.locator('polyline')).toHaveCount(2);
  await expect(opencodeChart.locator('.dot')).toHaveCount(6);
  await expect(page.getByRole('table', { name: 'Недельный остаток по дням' }).getByRole('row')).toHaveCount(22);
  // Обновление стоит у заголовка «Сейчас» рядом со временем показания.
  await expect(page.locator('main .now .meta')).toHaveText(/^обновлено \d/);
  await expect(page.locator('main').getByRole('button', { name: 'Обновить лимиты' })).toBeVisible();
});

test('empty_history_usage_and_speed_each_say_so_in_place', async ({ page }) => {
  await bootRealisticLimits(page, { history: 'empty' });
  const main = page.locator('main');
  await expect(main.locator('[data-history-empty]')).toHaveText('Снимков лимитов за эти дни ещё нет');
  await expect(main.getByRole('img', { name: /Недельный остаток по дням/ })).toHaveCount(0);
  await expect(main.getByRole('table', { name: 'Сегодня по сессиям' })).toHaveCount(0);
  await expect(main.getByText('Сегодня завершённых запусков ещё нет')).toBeVisible();
  await expect(main.getByRole('table', { name: 'Скорость моделей' })).toHaveCount(0);
  await expect(main.getByText('За неделю ещё нет ходов с измеренной скоростью')).toBeVisible();
  // Текущие лимиты от истории не зависят.
  await expect(main.locator('.current .provider')).toHaveCount(3);
  expect(await overflowing(page)).toEqual([]);
});

test('every_palette_keeps_limits_text_readable_in_both_themes', async ({ page }) => {
  test.setTimeout(180_000);
  await bootRealisticLimits(page);
  const failures: string[] = [];
  for (const palette of PALETTES) {
    for (const theme of ['dark', 'light']) {
      await page.evaluate(([p, t]) => { localStorage.setItem('canterlot.palette', p!); localStorage.setItem('canterlot.theme', t!); }, [palette.key, theme]);
      await page.reload();
      await page.locator('main[data-secondary-ready]').waitFor();
      await expect(page.locator('html')).toHaveAttribute('data-palette', palette.key);
      const low = await page.locator('main .secondary-content').evaluate((content) => {
        const rgb = (value: string) => (value.match(/[\d.]+/g) ?? []).map(Number);
        const opaque = (value: string) => { const parts = rgb(value); return parts.length >= 3 && (parts[3] ?? 1) > 0; };
        const luminance = ([r, g, b]: number[]) => [r!, g!, b!].map((c) => { const s = c / 255; return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; }).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i]!, 0);
        const found: string[] = [];
        for (const el of content.querySelectorAll<HTMLElement>('*')) {
          if (el.closest('.sr-only')) continue;
          if (![...el.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && (node.textContent ?? '').trim())) continue;
          const style = getComputedStyle(el);
          if (style.display === 'none' || style.visibility === 'hidden') continue;
          let under = getComputedStyle(document.documentElement).backgroundColor;
          for (let node: HTMLElement | null = el; node; node = node.parentElement) { const bg = getComputedStyle(node).backgroundColor; if (opaque(bg)) { under = bg; break; } }
          const [a, b] = [luminance(rgb(style.color)), luminance(rgb(under))].sort((x, y) => y - x);
          const ratio = (a! + 0.05) / (b! + 0.05);
          const size = Number.parseFloat(style.fontSize);
          const large = size >= 24 || (size >= 18.66 && Number(style.fontWeight) >= 700);
          if (ratio < (large ? 3 : 4.5)) found.push(`${el.tagName.toLowerCase()}.${String(el.className).split(' ')[0]} «${(el.textContent ?? '').trim().slice(0, 20)}» ${ratio.toFixed(2)}`);
        }
        return [...new Set(found)];
      });
      for (const item of low) failures.push(`${palette.key} ${theme}: ${item}`);
    }
  }
  expect(failures).toEqual([]);
});
