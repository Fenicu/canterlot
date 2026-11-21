import type { Page } from '@playwright/test';
import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

/**
 * Уход поверхностей: лист, меню и окно «Отправить позже» закрываются сразу, а уход только дорисовывается.
 * Лист рисует его дискретным CSS-переходом (`display` и `overlay`), которого в Firefox нет: там лист
 * закрывается мгновенно, как раньше. Меню и окно убираются из документа, и их уход держит `ui/exit.ts`.
 */

const SLOW = 20;

async function slow(page: Page, scale = SLOW) {
  await page.evaluate((value) => document.documentElement.style.setProperty('--motion-scale', String(value)), scale);
}

async function chat(page: Page) {
  const { ids } = await bootSecondary(page);
  await page.goto(`/c/${ids.pinned}`);
  const field = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(field).toBeVisible();
  return field;
}

const overlayDrawn = (page: Page) => page.evaluate(() => CSS.supports('overlay', 'auto'));

/** Лист закрыт и страница под ним свободна с первого кадра, а не после ухода. */
const closedAtOnce = (page: Page) => page.evaluate(() => ({
  open: document.querySelectorAll('dialog[open]').length,
  inert: document.getElementById('app-shell')?.inert ?? false
}));

test('sheet_exit_closes_at_once_and_is_drawn_where_overlay_is_supported', async ({ page }) => {
  const field = await chat(page);
  await page.getByRole('button', { name: 'Заготовки' }).click();
  const sheet = page.getByRole('dialog');
  await expect(sheet).toBeVisible();
  const drawn = await overlayDrawn(page);
  // Вход доигран на обычной скорости, а уход смотрим замедленным: срез застаёт его на месте.
  await expect.poll(() => sheet.evaluate((el) => el.getAnimations().length)).toBe(0);
  const resting = await sheet.evaluate((el) => el.getBoundingClientRect().top);
  await slow(page);

  await page.keyboard.press('Escape');
  // Одно нажатие: диалог уже закрыт, страница свободна, фокус вернулся.
  expect(await closedAtOnce(page)).toEqual({ open: 0, inert: false });
  await expect(field).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Заготовки' })).toBeFocused();
  // Уходящий лист скрыт для чтения с экрана и ничего не принимает.
  await expect(sheet).toHaveCount(0);

  const leaving = page.locator('dialog:not([open])');
  if (drawn) {
    await expect.poll(() => leaving.evaluate((el) => ({ display: getComputedStyle(el).display, top: el.getBoundingClientRect().top, events: getComputedStyle(el).pointerEvents }))
      .then((state) => state.display !== 'none' && state.top > resting && state.events === 'none'), { message: 'лист едет вниз, а не стоит и не исчез' }).toBe(true);
  }
  // Уход доигрывается и узел уходит в покой: ничего не висит открытым.
  await expect.poll(() => leaving.evaluate((el) => getComputedStyle(el).display), { timeout: 10_000 }).toBe('none');
});

for (const mode of ['reduced', 'scale_zero'] as const) {
  test(`sheet_and_menu_close_at_once_without_motion_${mode}`, async ({ page }) => {
    await chat(page);
    if (mode === 'reduced') await page.emulateMedia({ reducedMotion: 'reduce' });
    else await slow(page, 0);

    await page.getByRole('button', { name: 'Заготовки' }).click();
    await expect(page.getByRole('dialog')).toBeVisible();
    await page.keyboard.press('Escape');
    // Без движения уход равен нулю: закрытый лист не виден уже в следующем кадре.
    expect(await page.evaluate(() => getComputedStyle(document.querySelector('dialog')!).display)).toBe('none');

    await page.getByRole('button', { name: 'Меню сессии' }).click();
    await expect(page.getByRole('menu', { name: 'Действия над сессией' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('[role="menu"]')).toHaveCount(0);
  });
}

test('menu_exit_mirrors_the_entrance_and_takes_nothing_while_leaving', async ({ page }) => {
  await chat(page);
  const button = page.getByRole('button', { name: 'Меню сессии' });
  await button.click();
  const menu = page.getByRole('menu', { name: 'Действия над сессией' });
  await expect(menu).toBeVisible();
  await expect.poll(() => menu.evaluate((el) => el.getAnimations().length)).toBe(0);
  await slow(page);

  await page.keyboard.press('Escape');
  const leaving = page.locator('[role="menu"][data-leaving]');
  await expect(leaving).toHaveCount(1);
  await expect(leaving).toHaveAttribute('aria-hidden', 'true');
  expect(await leaving.evaluate((el) => (el as HTMLElement).inert)).toBe(true);
  // Уходящее меню не мешает открыть новое и не попадает под запрос по роли.
  await expect(menu).toHaveCount(0);
  await expect.poll(() => leaving.evaluate((el) => Number(getComputedStyle(el).opacity))).toBeLessThan(1);
  await expect(leaving).toHaveCount(0, { timeout: 10_000 });
});

test('send_later_exit_is_drawn_for_the_popover_and_the_sheet', async ({ page, isMobile }) => {
  const field = await chat(page);
  await field.fill('позже');
  await field.press('Control+Shift+Enter');
  const panel = page.getByRole('dialog', { name: 'Отправить позже' });
  await expect(panel).toBeVisible();
  await expect.poll(() => panel.evaluate((el) => el.getAnimations().length)).toBe(0);
  await slow(page);

  await page.keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
  if (isMobile) {
    expect(await closedAtOnce(page)).toEqual({ open: 0, inert: false });
    if (await overlayDrawn(page)) {
      await expect(page.locator('dialog:not([open])')).toHaveCSS('display', /flex/);
    }
  } else {
    await expect(page.locator('.popover[data-leaving]')).toHaveCount(1);
  }
  await expect(page.locator('.popover[data-leaving]')).toHaveCount(0, { timeout: 10_000 });
  // Повторное открытие работает: окно не осталось заперто уходом.
  await field.press('Control+Shift+Enter');
  await expect(page.getByRole('dialog', { name: 'Отправить позже' })).toBeVisible();
});

test('route_backed_sheet_stays_in_the_document_until_its_exit_is_drawn', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'на рабочем столе новая сессия не лист, а обычное окно');
  await bootSecondary(page, '/new');
  const sheet = page.getByRole('dialog', { name: 'Новая сессия' });
  await expect(sheet).toBeVisible();
  await expect.poll(() => sheet.evaluate((el) => el.getAnimations().length)).toBe(0);
  await slow(page);

  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/\/sessions$/);
  // Страница ушла, а лист ещё дорисовывает уход: его держит ожидание ухода, а не отложенная навигация.
  if (await overlayDrawn(page)) {
    await expect(page.locator('dialog:not([open])')).toHaveCount(1);
    expect(await page.locator('dialog:not([open])').evaluate((el) => getComputedStyle(el).display)).not.toBe('none');
  }
  await expect(page.locator('dialog')).toHaveCount(0, { timeout: 10_000 });
});
