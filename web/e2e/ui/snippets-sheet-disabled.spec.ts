import { expect, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

/** «Сохранить как заготовку» при пустом поле: кнопка выглядит выключенной, а причина стоит под ней. */
test('snippets_sheet_disabled_save_has_visible_state_and_hint_below', async ({ page }) => {
  const { ids } = await bootSecondary(page, '/settings');
  await page.goto(`/c/${ids.pinned}`);
  const field = page.getByRole('textbox', { name: 'Сообщение' });
  await expect(field).toBeVisible();
  await page.getByRole('button', { name: 'Заготовки' }).click();
  const sheet = page.getByRole('dialog');
  const save = sheet.getByRole('button', { name: 'Сохранить как заготовку' });
  await expect(save).toBeDisabled();
  const reason = sheet.getByText('Поле ввода пусто');
  await expect(reason).toBeVisible();

  if (process.env.SNIP_SHOTS) await sheet.screenshot({ path: `${process.env.SNIP_SHOTS}-${test.info().project.name}.png` });
  const geometry = await save.evaluate((button) => {
    const hint = document.getElementById(button.getAttribute('aria-describedby') ?? '')!;
    const b = button.getBoundingClientRect();
    const h = hint.getBoundingClientRect();
    const line = parseFloat(getComputedStyle(hint).lineHeight) || parseFloat(getComputedStyle(hint).fontSize) * 1.4;
    return {
      buttonLeft: b.left, buttonBottom: b.bottom, hintLeft: h.left, hintTop: h.top, hintHeight: h.height, line,
      opacity: parseFloat(getComputedStyle(button).opacity)
    };
  });
  expect(geometry.hintTop, 'причина стоит под кнопкой, а не справа').toBeGreaterThanOrEqual(geometry.buttonBottom - 0.5);
  expect(Math.abs(geometry.hintLeft - geometry.buttonLeft), 'причина выровнена по левому краю кнопки').toBeLessThanOrEqual(12);
  expect(geometry.hintHeight, 'причина в одну строку').toBeLessThanOrEqual(geometry.line * 1.5);
  expect(geometry.opacity, 'выключенная кнопка заметно бледнее включённой').toBeLessThanOrEqual(0.6);

  await sheet.getByRole('button', { name: 'Закрыть' }).first().click().catch(() => page.keyboard.press('Escape'));
  await field.fill('Текст заготовки');
  await page.getByRole('button', { name: 'Заготовки' }).click();
  const enabled = page.getByRole('dialog').getByRole('button', { name: 'Сохранить как заготовку' });
  await expect(enabled).toBeEnabled();
  expect(await enabled.evaluate((button) => parseFloat(getComputedStyle(button).opacity))).toBe(1);
});
