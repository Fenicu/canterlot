import { test, expect } from './fixtures';
import { bootSecondary } from './secondary-fixture';
import { machineWatchSeed, seedMachineWatch } from '../../testing/fake-hub';

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

test('machine_screen_shows_the_watchdog_report_from_the_snapshot', async ({ page }) => {
  const { api, ids } = await bootSecondary(page);
  seedMachineWatch(api, ids.machine);
  await page.reload();
  await expect(page.locator('[data-secondary-ready]')).toBeVisible({ timeout: 15_000 });
  const main = page.locator('main');
  await expect(main.locator('[data-watch="memory"]')).toHaveText('Мало памяти: 2,1 ГиБ свободно');
  await expect(main.getByRole('meter', { name: 'RAM' })).toHaveAttribute('data-tone', 'warning');
  const caches = main.getByRole('region', { name: 'Крупные кэши сборки' });
  // Самый крупный каталог первым, рабочая копия делегата названа по сессии.
  const rows = caches.getByRole('listitem');
  await expect(rows).toHaveCount(4);
  await expect(rows.first()).toContainText('/home/owner/myprojects/canterlot-2a/target');
  await expect(rows.first()).toContainText('77 ГБ');
  await expect(rows.first()).toContainText('из них incremental 31 ГБ · запись 2 ч назад');
  await expect(rows.nth(1)).toContainText('Рабочая копия «Баннер связи»');
  await expect(rows.nth(1)).toContainText('…/01a1b8e2-659f-715f-aad1-bf53d98aefdd/target');
  await expect(rows.nth(1)).toContainText('запись 5 ч назад');
  await expect(rows.nth(2)).toContainText('1,2 ГБ');
  await expect(rows.nth(2)).not.toContainText('incremental');
  await expect(caches).toContainText('Освободить: scripts/prune-target.sh или удалить target/debug/incremental в простаивающих копиях');
  const leaked = main.getByRole('region', { name: '3 процесса пережили свои сессии' });
  await expect(leaked).toContainText('cargo 2, node 1 · занимают 1,4 ГиБ');
  // Процессы одного запуска стоят под одним названием сессии.
  await expect(leaked.getByRole('listitem')).toHaveCount(2);
  await expect(leaked.getByRole('listitem').first()).toContainText('Баннер связи');
  await expect(leaked.getByRole('listitem').first()).toContainText('cargo · pid 48211 · 3 ч · 600 МиБ');
  await expect(leaked.getByRole('listitem').first()).toContainText('cargo · pid 48390 · 3 ч · 380 МиБ');
  await expect(leaked.getByRole('listitem').last()).toContainText('Запуск d4a7c3f9');
  await expect(main.locator('[data-watch="unleased"]')).toHaveText('Без очереди: 2 сборки');
  // Длинные пути переносятся внутри колонки, а не раздвигают страницу.
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  const box = await caches.boundingBox();
  const view = page.viewportSize()!;
  expect(box!.x + box!.width).toBeLessThanOrEqual(view.width);
});

test('machine_screen_applies_machine_watch_changed_live', async ({ page }) => {
  const { hub, api, ids } = await bootSecondary(page);
  const main = page.locator('main');
  await expect(main.getByRole('heading', { name: 'workstation' })).toBeVisible();
  await expect(main.getByRole('region', { name: 'Крупные кэши сборки' })).toHaveCount(0);
  const seed = machineWatchSeed(api);
  hub.append('machines', { kind: 'machine_watch_changed', payload: { machine_id: ids.machine, watch: seed.watch, memory_low: true } }); hub.flush();
  await expect(main.getByRole('region', { name: 'Крупные кэши сборки' }).getByRole('listitem')).toHaveCount(4);
  await expect(main.getByRole('region', { name: '3 процесса пережили свои сессии' })).toBeVisible();
  await expect(main.locator('[data-watch="memory"]')).toContainText('Мало памяти');
  await expect(main.locator('[data-watch="unleased"]')).toHaveText('Без очереди: 2 сборки');
  // Следующий отчёт заменяет прежний целиком: процессы ушли, память вернулась, сборка одна.
  hub.append('machines', { kind: 'machine_watch_changed', payload: { machine_id: ids.machine, watch: { ...seed.watch, leaked: [], unleased_count: 1 }, memory_low: false } }); hub.flush();
  await expect(main.locator('[data-watch="unleased"]')).toHaveText('Без очереди: 1 сборка');
  await expect(main.getByRole('region', { name: /пережил/ })).toHaveCount(0);
  await expect(main.locator('[data-watch="memory"]')).toHaveCount(0);
  hub.append('machines', { kind: 'machine_watch_changed', payload: { machine_id: ids.machine, watch: null, memory_low: false } }); hub.flush();
  await expect(main.getByRole('region', { name: 'Крупные кэши сборки' })).toHaveCount(0);
  await expect(main.locator('[data-watch="unleased"]')).toHaveCount(0);
});
