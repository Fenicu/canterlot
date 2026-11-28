import type { Page } from '@playwright/test';
import { seedCloud, seedJob, seedMachine } from '../../testing/fake-hub';
import type { CloudSeed } from '../../testing/fake-hub/cloud';
import { bootSecondary } from './secondary-fixture';
import { expect, test } from './fixtures';

// Проверка геометрии в браузере: jsdom не считает переносы и размеры текста.
test.use({ reducedMotion: 'reduce' });

const STATES: { name: string; cloud: CloudSeed }[] = [
  { name: 'без окна', cloud: { live: false, ended: 0, card: false } },
  { name: 'окно создаётся', cloud: { live: { state: 'create_intent', ready_at: null }, preemption: false, ended: 0, card: false } },
  { name: 'окно готово, замена прерванного', cloud: { live: {}, ended: 2, card: false } },
  { name: 'окно удаляется', cloud: { live: { state: 'deleting', delete_reason: 'owner' }, preemption: false, ended: 0, card: false } },
  { name: 'законченные окна', cloud: { live: false, ended: 3, card: false } },
  { name: 'открыта карточка', cloud: { live: false, ended: 1, card: {} } }
];

async function bootMachines(page: Page, cloud: CloudSeed) {
  const booted = await bootSecondary(page, '/machines', (api) => {
    const carrier = [...api.machines.values()].find((machine) => machine.name === 'workstation')!;
    seedMachine(api, { name: 'cloud', role: 'runner', parent_machine_id: carrier.id, rank: 10, labels: ['cloud', 'gate'], availability: 'online', observed_at: null, accepts_remote_work: true, harnesses: [] });
    seedMachine(api, { name: 'dev-vm' });
    // Гейт на раннере по точному коммиту: строка источника длинная и без пробелов внутри.
    const source = { repo: 'https://git.example.com/lighthouse/lighthouse.git', rev: '0000000000000000000000000000000000000000' };
    for (const machine of [...api.machines.values()]) {
      seedJob(api, { id: machine.id, name: machine.name }, { state: 'succeeded', title: 'Гейт lighthouse на точном коммите', lease: 'gate', source, exit_code: 0, duration_ms: 1_234_000, started_at: new Date().toISOString(), finished_at: new Date().toISOString() });
    }
    seedCloud(api, cloud);
  });
  await expect(page.locator('[data-cloud-section] h2')).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('[data-cloud-section]')).not.toContainText('Загружаю облачные окна');
  return booted;
}

/** Ширина прокрутки тела экрана и самые глубокие элементы, чей текст или коробка вылезли за его правый край. */
async function overflow(page: Page) {
  return page.locator('.secondary-body').evaluate((body) => {
    const edge = body.getBoundingClientRect().right;
    const wide = [...body.querySelectorAll('*')]
      .map((element) => {
        // Текст при nowrap вылезает из своей коробки, а коробка остаётся в ширине строки: меряем и текст.
        const range = document.createRange();
        range.selectNodeContents(element);
        return { element, right: Math.max(element.getBoundingClientRect().right, range.getBoundingClientRect().right) };
      })
      .filter(({ right }) => right > edge + 0.5)
      .reverse()
      .map(({ element, right }) => `${element.tagName.toLowerCase()}.${[...element.classList].join('.')} «${(element.textContent ?? '').trim().slice(0, 40)}» +${Math.round(right - edge)}px`);
    return { scrollWidth: body.scrollWidth, clientWidth: body.clientWidth, wide: wide.slice(0, 8) };
  });
}

for (const state of STATES) {
  test(`экран машин не прокручивается вбок на телефоне: ${state.name}`, async ({ page }) => {
    await bootMachines(page, state.cloud);
    const history = page.locator('[data-cloud-history] summary');
    if (await history.count()) await history.click();
    const tabs = page.getByRole('radiogroup', { name: 'Машина' }).locator('label');
    const count = await tabs.count();
    expect(count, 'вкладок машин меньше, чем машин').toBeGreaterThanOrEqual(4);
    for (let index = 0; index < count; index += 1) {
      await tabs.nth(index).click();
      const name = (await tabs.nth(index).textContent())?.trim() ?? '';
      await expect(page.locator('.heading h2')).toHaveText(name.split(' · ')[0]!);
      await expect(page.locator('[data-machine-jobs] [data-job]').first()).toBeVisible();
      const box = await overflow(page);
      expect(box.scrollWidth, `вкладка ${name}: содержимое можно прокрутить вбок, вылезли: ${box.wide.join('; ')}`).toBeLessThanOrEqual(box.clientWidth);
    }
  });
}

test('под кнопкой «Добавить машину» есть зазор до вкладок машин', async ({ page }) => {
  await bootMachines(page, { live: false, ended: 0, card: false });
  const add = (await page.getByRole('button', { name: 'Добавить машину' }).boundingBox())!;
  const tabs = (await page.getByRole('radiogroup', { name: 'Машина' }).boundingBox())!;
  // Наименьший зазор вертикального ритма экрана: тот же, что между карточками.
  const gap = await page.evaluate(() => {
    const probe = document.createElement('div');
    probe.style.height = 'var(--s4)';
    document.body.append(probe);
    const value = probe.getBoundingClientRect().height;
    probe.remove();
    return value;
  });
  expect(gap).toBeGreaterThan(0);
  expect(tabs.y - (add.y + add.height), 'кнопка прилипла к вкладкам').toBeGreaterThanOrEqual(gap - 0.5);
});
