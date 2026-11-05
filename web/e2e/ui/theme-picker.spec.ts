import { PALETTES } from '../../src/lib/palettes';
import { bootSecondary } from './secondary-fixture';
import { expect, test } from './fixtures';

test('picker_shows_every_palette_with_a_live_preview', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Тема' });
  await expect(group.getByRole('radio')).toHaveCount(PALETTES.length);
  for (const { key, name } of PALETTES) await expect(group.getByRole('radio', { name, exact: true })).toHaveValue(key);
  await expect(group.getByRole('radio', { name: 'Стандартная', exact: true })).toBeChecked();
  // Каждая карточка рисует себя своими токенами: фоны предпросмотров различаются, а не повторяют фон приложения.
  const backgrounds = await group.locator('.preview').evaluateAll(nodes => nodes.map(node => getComputedStyle(node).backgroundColor));
  expect(new Set(backgrounds).size).toBe(PALETTES.length);
  const app = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--chat-bg').trim());
  const sepia = group.locator('[data-option=sepia] .preview');
  await expect(sepia).toHaveAttribute('data-theme', 'dark');
  expect(await sepia.evaluate(node => getComputedStyle(node).getPropertyValue('--chat-bg').trim())).not.toBe(app);
  // Карточка — цель нажатия не меньше 44px, предпросмотр не уходит за края на узком экране.
  for (const box of await group.locator('label').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().toJSON() as DOMRect))) {
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(box.right).toBeLessThanOrEqual(page.viewportSize()!.width);
  }
  // Предпросмотр следует режиму приложения.
  await page.locator('main').getByRole('radio', { name: 'Светлый', exact: true }).check();
  await expect(sepia).toHaveAttribute('data-theme', 'light');
});

test('arrow_keys_choose_a_palette_and_the_choice_survives_reload', async ({ page }) => {
  await bootSecondary(page, '/settings');
  const group = page.locator('main').getByRole('radiogroup', { name: 'Тема' });
  const first = group.getByRole('radio', { name: 'Стандартная', exact: true });
  await first.focus();
  await page.keyboard.press('ArrowRight');
  await expect(group.getByRole('radio', { name: 'Аквамарин', exact: true })).toBeChecked();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'aquamarine');
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('ArrowDown');
  await expect(group.getByRole('radio', { name: 'Чернила', exact: true })).toBeChecked();
  await expect(group.getByRole('radio', { name: 'Чернила', exact: true })).toBeFocused();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'ink');
  await page.keyboard.press('ArrowLeft');
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'vscode');
  // Выбор видно не только цветом: у карточки кольцо, у метки заполненный круг, подпись жирнее.
  const chosen = group.locator('[data-option=vscode]');
  const other = group.locator('[data-option=ink]');
  const style = (el: typeof chosen) => el.evaluate(node => ({
    outline: getComputedStyle(node).outlineStyle,
    dot: getComputedStyle(node.querySelector('.mark')!, '::after').inset,
    weight: getComputedStyle(node.querySelector('.name')!).fontWeight
  }));
  const [on, off] = [await style(chosen), await style(other)];
  expect(on.outline).toBe('solid');
  expect(off.outline).toBe('none');
  expect(on.dot).not.toBe(off.dot);
  expect(on.weight).not.toBe(off.weight);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme-switching', '');
  expect(await page.evaluate(() => localStorage.getItem('canterlot.palette'))).toBe('vscode');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-palette', 'vscode');
  await expect(page.locator('main').getByRole('radio', { name: 'VS Code', exact: true })).toBeChecked();
  const meta = await page.evaluate(() => [document.querySelector('meta[name="theme-color"]')?.getAttribute('content'), getComputedStyle(document.documentElement).getPropertyValue('--bg').trim()]);
  expect(meta[0]).toBe(meta[1]);
});
