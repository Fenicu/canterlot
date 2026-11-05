import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';

test('machine_screen_follows_mockup_09_and_updates_metrics_live', async ({ page }) => {
  const { hub, ids, metrics } = await bootSecondary(page);
  const main = page.locator('main');
  await expect(main.getByRole('heading', { name: 'workstation' })).toBeVisible();
  await expect(main.getByText('В сети', { exact: true })).toBeVisible();
  await expect(main.getByText('Linux · x86_64')).toBeVisible();
  await expect(main.getByRole('meter', { name: 'CPU' })).toHaveAttribute('aria-valuenow', '24');
  await expect(main.getByRole('meter', { name: 'RAM' })).toHaveAttribute('aria-valuenow', '40');
  await expect(main.getByRole('meter')).not.toHaveCount(0);
  expect(await main.getByRole('meter').count()).toBeGreaterThan(2);
  // Шкалы всех карточек начинаются с одного края и одной ширины, даже когда подпись диска переносится.
  const gauges = await main.getByRole('meter').evaluateAll((nodes) => nodes.map((node) => { const box = node.getBoundingClientRect(); return [Math.round(box.left), Math.round(box.width)]; }));
  expect(new Set(gauges.map(([left]) => left)).size).toBe(1);
  expect(new Set(gauges.map(([, width]) => width)).size).toBe(1);
  await expect(main.getByText(/Последний сигнал/)).toBeVisible();
  await expect(main.getByText(/Подключение с/)).toBeVisible();
  await expect(main.getByRole('link', { name: /^Восстановление потока/ })).toHaveAttribute('href', `/c/${ids.pinned}`);
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, cpu_permille: 730, mem_available_bytes: metrics.mem_total_bytes / 2 } }); hub.flush();
  await expect(main.getByRole('meter', { name: 'CPU' })).toHaveAttribute('aria-valuenow', '73');
  await expect(main.getByRole('meter', { name: 'RAM' })).toHaveAttribute('aria-valuenow', '50');
  // Состояние машины — текст, а не капсула-кнопка; нестабильная машина отмечена уже в переключателе.
  await expect(main.locator('.availability')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');
  await main.getByRole('radio', { name: 'mini · нестабильно', exact: true }).check();
  await expect(main.getByText('Связь нестабильна', { exact: true })).toBeVisible();
});

test('machine_screen_flags_disk_warning_and_stale_data', async ({ page }, testInfo) => {
  const { hub, ids, metrics } = await bootSecondary(page);
  const disk = page.getByRole('meter', { name: 'Данные воркера' });
  await expect(disk).toHaveClass(/warn/);
  await expect(page.locator('main').getByText('Свободно 24 ГБ из 295')).toBeVisible();
  await expect(page.locator('main').getByText('Мало свободного места на диске')).toBeVisible();
  if (process.env.DISK_WARN_SCREENSHOTS) {
    const { mkdirSync } = await import('node:fs');
    mkdirSync('/tmp/disk-warn', { recursive: true });
    await page.screenshot({ path: `/tmp/disk-warn/${testInfo.project.name}-machines.png`, fullPage: true });
  }
  await page.clock.install();
  await page.clock.fastForward(92_000);
  await expect(page.locator('[data-stale="true"]')).toContainText('данные устарели');
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: 36_000_000_000 }] } }); hub.flush();
  await expect(disk).not.toHaveClass(/warn/);
  await expect(page.locator('main').getByText('Свободно 36 ГБ из 295')).toBeVisible();
  await expect(page.locator('main').getByText('Мало свободного места на диске')).toHaveCount(0);
});

test('machine_screen_uses_danger_tone_below_the_critical_threshold', async ({ page }) => {
  const { hub, ids, metrics } = await bootSecondary(page);
  const disk = page.getByRole('meter', { name: 'Данные воркера' });
  await expect(disk).toHaveAttribute('data-tone', 'warning');
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: 8_000_000_000 }] } }); hub.flush();
  await expect(disk).toHaveAttribute('data-tone', 'danger');
  await expect(page.locator('main').getByText('Свободно 8 ГБ из 295')).toBeVisible();
});

test('sessions_list_shows_a_quiet_low_disk_notice_with_hysteresis', async ({ page }, testInfo) => {
  const { hub, ids, metrics } = await bootSecondary(page, '/sessions');
  const notice = page.locator('.disk-notice');
  await expect(notice).toHaveText('На workstation мало места: свободно 24 ГБ');
  await expect(notice).toHaveAttribute('data-tone', 'warning');
  if (process.env.DISK_WARN_SCREENSHOTS) {
    const { mkdirSync } = await import('node:fs');
    mkdirSync('/tmp/disk-warn', { recursive: true });
    await page.screenshot({ path: `/tmp/disk-warn/${testInfo.project.name}-sessions.png`, fullPage: true });
  }
  // Ниже порога с запасом (35 ГБ) строка ещё держится.
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: 34_000_000_000 }] } }); hub.flush();
  await expect(notice).toHaveText('На workstation мало места: свободно 34 ГБ');
  // Ниже критического порога — тон меняется на опасный.
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: 8_000_000_000 }] } }); hub.flush();
  await expect(notice).toHaveAttribute('data-tone', 'danger');
  // Возврат выше порога с запасом убирает строку без сообщения.
  hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: ids.machine, ...metrics, disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: 36_000_000_000 }] } }); hub.flush();
  await expect(notice).toHaveCount(0);
});

test('machines_and_limits_have_no_horizontal_scroll_at_360px', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await bootSecondary(page);
  for (const route of ['/machines', '/limits']) {
    await page.goto(route);
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= 360)).toBe(true);
  }
});

test('secondary_screens_reflow_with_large_text_and_support_the_light_theme', async ({ page }) => {
  await bootSecondary(page);
  for (const route of ['/machines', '/limits', '/settings']) {
    await page.goto(route);
    await expect(page.locator('[data-secondary-ready]')).toBeVisible();
    const toggle = page.locator('main').getByRole('button', { name: 'Сменить тему' });
    if (await page.locator('html').getAttribute('data-theme') !== 'light') await toggle.click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
    for (const width of [360, 320]) {
      await page.setViewportSize({ width, height: 800 });
      await page.evaluate(() => { document.documentElement.style.fontSize = '32px'; });
      const size = await page.evaluate(() => ({ page: document.documentElement.scrollWidth, width: document.documentElement.clientWidth, outside: [...document.querySelectorAll('main *')].map(node => ({ tag: node.tagName, cls: node.className, right: node.getBoundingClientRect().right, width: node.getBoundingClientRect().width })).filter(node => node.right > document.documentElement.clientWidth + 1).slice(0, 12) }));
      expect(size.page, `${route} at ${width}: ${JSON.stringify(size)}`).toBeLessThanOrEqual(size.width);
    }
    await page.evaluate(() => { document.documentElement.style.removeProperty('font-size'); });
    if (process.env.TASK9B_SCREENSHOTS) await page.screenshot({ path: `/tmp/task9b-${route.slice(1)}.png` });
  }
});

test('secondary_navigation_matches_phone_and_desktop_mockups', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await bootSecondary(page);
  await expect(page.getByRole('navigation', { name: 'Навигация' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Новая сессия' })).toHaveCount(0);
  await page.goto('/settings');
  await expect(page.locator('main').getByRole('heading', { name: 'Настройки' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Навигация' })).toHaveCount(0);
  await page.setViewportSize({ width: 1440, height: 900 });
  await expect(page.locator('[data-pane="list"]').getByRole('navigation', { name: 'Навигация' })).toBeVisible();
  await page.locator('[data-pane="list"]').getByRole('link', { name: 'Машины', exact: true }).click();
  await expect(page.locator('main').getByRole('heading', { name: 'Машины', exact: true })).toBeVisible();
});
