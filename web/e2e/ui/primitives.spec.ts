import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { productionCsp } from '../../csp.mjs';
test.beforeEach(async ({ page }) => { await page.goto('/__test/primitives'); });
test('primitives_have_visible_focus_and_44px_targets', async ({ page }) => {
  for (const name of ['Открыть панель', 'Информация', 'Сменить тему']) {
    await page.keyboard.press('Tab');
    const button = page.getByRole('button', { name, exact: true }); await button.focus();
    const style = await button.evaluate(node => { const css = getComputedStyle(node); return { width: node.getBoundingClientRect().width, height: node.getBoundingClientRect().height, outline: css.outlineWidth, token: css.getPropertyValue('--focus-width'), target: parseFloat(css.getPropertyValue('--target')) }; });
    expect(style.width).toBeGreaterThanOrEqual(style.target); expect(style.height).toBeGreaterThanOrEqual(style.target); expect(parseFloat(style.outline)).toBe(parseFloat(style.token));
  }
  const input = page.getByLabel('Название', { exact: true });
  const inputStyle = await input.evaluate(node => { const css = getComputedStyle(node); return { width: node.getBoundingClientRect().width, height: node.getBoundingClientRect().height, target: parseFloat(css.getPropertyValue('--target')) }; });
  expect(inputStyle.height).toBeGreaterThanOrEqual(inputStyle.target);
  expect(inputStyle.width).toBeGreaterThanOrEqual(inputStyle.target);
  await expect(page.getByRole('button', { name: 'Отправить', exact: true })).toHaveAccessibleDescription('Сначала подключите машину');
});
test('sheet_traps_focus_closes_on_escape_and_restores_focus', async ({ page }) => {
  const opener = page.getByRole('button', { name: 'Открыть панель', exact: true }); await opener.click();
  const dialog = page.getByRole('dialog', { name: 'Параметры' }); await expect(dialog).toBeVisible();
  await expect(page.locator('#app-shell')).toHaveJSProperty('inert', true);
  await dialog.getByRole('button', { name: 'Готово' }).focus(); await page.keyboard.press('Tab'); await expect(dialog.getByRole('button', { name: 'Закрыть' })).toBeFocused();
  await page.keyboard.press('Shift+Tab'); await expect(dialog.getByRole('button', { name: 'Готово' })).toBeFocused();
  await page.keyboard.press('Escape'); await expect(dialog).not.toBeVisible(); await expect(opener).toBeFocused();
  await expect(page.locator('#app-shell')).toHaveJSProperty('inert', false);
});
test('back_closes_sheet_and_keeps_scroll_position', async ({ page }) => {
  const opener = page.getByRole('button', { name: 'Открыть панель', exact: true });
  await page.evaluate(() => window.scrollTo(0, 180));
  const before = await page.evaluate(() => scrollY);
  expect(before).toBeGreaterThan(0);
  await opener.dispatchEvent('click');
  await expect(page.getByRole('dialog', { name: 'Параметры' })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole('dialog', { name: 'Параметры' })).not.toBeVisible();
  expect(await page.evaluate(() => scrollY)).toBe(before);
});
test('back_closes_confirm_dialog_via_history', async ({ page }) => {
  await page.getByRole('button', { name: 'Запросить подтверждение' }).click();
  const dialog = page.getByRole('dialog', { name: 'Подтвердить действие' });
  await expect(dialog).toBeVisible();
  await page.goBack();
  await expect(dialog).not.toBeVisible();
});
test('segmented_is_a_keyboard_operable_radio_group', async ({ page }) => {
  const group = page.getByRole('radiogroup', { name: 'Режим' }); const first = group.getByRole('radio', { name: 'Первый', exact: true });
  await first.focus(); await page.keyboard.press('ArrowRight'); await expect(group.getByRole('radio', { name: 'Второй', exact: true })).toBeChecked(); await expect(page.getByTestId('selection')).toHaveText('two');
});
test('toggle_has_switch_semantics', async ({ page }) => {
  const toggle = page.getByRole('switch', { name: 'Автоматическое продолжение' }); await toggle.focus(); await page.keyboard.press('Space'); await expect(toggle).toBeChecked(); await page.keyboard.press('Space'); await expect(toggle).not.toBeChecked();
});
test('theme_toggle_persists_and_theme_script_runs_before_first_paint', async ({ page }) => {
  const shell = readFileSync('build/200.html', 'utf8'); expect(shell.slice(shell.indexOf('<head>')).match(/<script\b[^>]*>/)?.[0]).toBe('<script src="/theme-init.js">');
  await page.getByRole('button', { name: 'Сменить тему' }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  expect(await page.evaluate(() => localStorage.getItem('canterlot.theme'))).toBe('light');
  await page.reload(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe('rgb(231, 242, 238)');
  await page.getByRole('button', { name: 'Сменить тему' }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});
test('built_shell_has_no_inline_event_handlers', () => expect(readFileSync('build/200.html', 'utf8')).not.toMatch(/\son\w+\s*=/i));
test('edge_server_sends_the_production_csp_header_from_the_single_source', async ({ request }) => {
  const shell = readFileSync('build/200.html', 'utf8');
  const hashes = [...shell.matchAll(/<script\b(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)].map(m => `sha256-${createHash('sha256').update(m[1]!).digest('base64')}`);
  for (const route of ['/', '/__test/primitives', '/missing.js']) expect((await request.get(route)).headers()['content-security-policy']).toBe(productionCsp({ websocketOrigin: 'ws://127.0.0.1:4173', scriptHashes: hashes }));
  const response = await request.post('/_share', { maxRedirects: 0 }); expect(response.status()).toBe(303); expect(response.headers()['location']).toBe('/share?error=sw_inactive');
});
test('stage_track_reflects_events_without_fake_progress', async ({ page }) => {
  const track = page.getByRole('group', { name: 'Доставка сообщения', exact: true }); await expect(track.locator('[data-state=active]')).toHaveCount(1);
  await page.waitForTimeout(1900); await expect(track.locator('[aria-current=step] .st-label')).toHaveText('сохранено');
  await page.getByRole('button', { name: 'Следующий этап' }).click(); await expect(track.locator('[aria-current=step] .st-label')).toHaveText('на машине'); await expect(track.locator('[data-state=done]')).toHaveCount(1);
  const file = page.getByRole('group', { name: 'Доставка вложения' }); await expect(file.locator('[aria-current=step] .st-ring-arc')).toHaveCSS('stroke-dasharray', /25(?:px)?, 100(?:px)?/);
  await page.getByRole('button', { name: 'Передано 65 процентов' }).click(); await expect(file.locator('[aria-current=step] .st-ring-arc')).toHaveCSS('stroke-dasharray', /65(?:px)?, 100(?:px)?/);
  await page.getByRole('button', { name: 'Завершить доставку' }).click(); await expect(track.locator('.st-status')).toHaveText('доставлено');
});
test('stage_track_is_static_under_reduced_motion', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await expect.poll(() => page.locator('.st').evaluateAll(nodes => nodes.flatMap(node => node.getAnimations({ subtree: true })).length)).toBe(0);
  await page.getByRole('button', { name: 'Следующий этап' }).click(); await expect(page.getByRole('group', { name: 'Доставка сообщения', exact: true }).locator('.st-status')).toHaveText('на машине');
  await page.emulateMedia({ reducedMotion: 'no-preference' }); await page.getByRole('button', { name: 'Без анимации' }).click();
  await expect.poll(() => page.locator('.st').evaluateAll(nodes => nodes.flatMap(node => node.getAnimations({ subtree: true })).length)).toBe(0);
  await page.getByRole('button', { name: 'Обычная' }).click();
});
test('stage_track_shows_error_reason', async ({ page }) => {
  await page.getByRole('button', { name: 'Ошибка этапа' }).click(); const track = page.getByRole('group', { name: 'Доставка сообщения', exact: true });
  await expect(track.locator('.st-status')).toHaveText('Обрыв передачи'); await expect(track.locator('[data-state=error]')).toHaveCount(1);
});
test('motion_scale_changes_all_computed_durations_live', async ({ page }) => {
  const getDurations = () => page.evaluate(() => {
    const probe = document.createElement('span');
    document.body.append(probe);
    const read = (token: string) => {
      probe.style.transitionDuration = `var(${token})`;
      return parseFloat(getComputedStyle(probe).transitionDuration);
    };
    const fillEl = document.querySelector('.st--compact .st-fill');
    const result = {
      fast: read('--duration-fast'),
      expand: read('--duration-expand'),
      arrive: read('--duration-stage-arrive'),
      progress: read('--duration-stage-progress'),
      fill: fillEl ? parseFloat(getComputedStyle(fillEl).transitionDuration) : 0
    };
    probe.remove();
    return result;
  });
  await expect.poll(async () => (await getDurations()).fast).toBeCloseTo(0.16);
  const normal = await getDurations();
  expect(normal.fast).toBeCloseTo(0.16);
  expect(normal.fill).toBeCloseTo(0.32);

  await page.getByRole('button', { name: 'Быстрее', exact: true }).click();
  await expect.poll(async () => (await getDurations()).fast).toBeCloseTo(0.08);
  const faster = await getDurations();
  expect(faster.fast).toBeCloseTo(0.08);
  expect(faster.expand).toBeCloseTo(0.12);
  expect(faster.arrive).toBeCloseTo(0.14);
  expect(faster.progress).toBeCloseTo(0.12);
  expect(faster.fill).toBeCloseTo(0.16);

  await page.getByRole('button', { name: 'Медленнее', exact: true }).click();
  await expect.poll(async () => (await getDurations()).fast).toBeCloseTo(0.24);
  const slower = await getDurations();
  expect(slower.fast).toBeCloseTo(0.24);
  expect(slower.expand).toBeCloseTo(0.36);
  expect(slower.arrive).toBeCloseTo(0.42);
  expect(slower.progress).toBeCloseTo(0.36);
  expect(slower.fill).toBeCloseTo(0.48);
});
test('primitives_reflow_in_both_themes', async ({ page }) => {
  for (const theme of ['dark', 'light']) for (const width of [360, 320]) {
    await page.setViewportSize({ width, height: 900 }); await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
    await page.evaluate(() => document.documentElement.style.fontSize = '200%'); expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
    await page.evaluate(() => document.documentElement.style.removeProperty('font-size'));
  }
});
test('all_primitive_controls_work_and_csp_has_no_violations', async ({ page }) => {
  await page.addInitScript(() => { (window as unknown as { violations: { directive: string; sample: string; source: string; line: number }[] }).violations = []; document.addEventListener('securitypolicyviolation', event => (window as unknown as { violations: { directive: string; sample: string; source: string; line: number }[] }).violations.push({ directive: event.violatedDirective, sample: event.sample, source: event.sourceFile, line: event.lineNumber })); });
  await page.reload(); await page.getByLabel('Название', { exact: true }).fill('Сессия'); await page.getByLabel('Вариант', { exact: true }).selectOption('two'); await page.getByLabel('Сообщение', { exact: true }).fill('Текст');
  await page.getByText('Подробности', { exact: true }).click(); await expect(page.getByText('Данные состояния')).toBeVisible();
  await page.getByRole('button', { name: 'Запросить подтверждение' }).click(); await page.getByRole('button', { name: 'Подтвердить', exact: true }).click(); await expect(page.getByText('Подтверждено', { exact: true })).toBeVisible();
  await expect(page.locator('[aria-live]')).toHaveCount(1);
  expect(await page.evaluate(() => (window as unknown as { violations: { directive: string; sample: string; source: string; line: number }[] }).violations)).toEqual([]);
});
