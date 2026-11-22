import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { seedMachine, type FakeHubApi } from '../../testing/fake-hub';
import { expect, openNewSessionForm, test } from './fixtures';
import { bootSecondary } from './secondary-fixture';

const OBSERVED_AGO_MS = 2 * 60_000 + 15_000;

/** Несущая `workstation` из посева и два раннера под ней; состояние каждого задают тесты. */
function seedRunners(api: FakeHubApi, states: { a: string; b?: string; observedA?: string | null } = { a: 'online' }) {
  const carrier = [...api.machines.values()].find((machine) => machine.name === 'workstation')!;
  carrier.is_carrier = true;
  seedMachine(api, {
    name: 'runner-a', role: 'runner', parent_machine_id: carrier.id, rank: 20, labels: ['build-host', 'kernel-6.12'],
    availability: states.a, observed_at: states.observedA === undefined ? new Date(Date.now() - OBSERVED_AGO_MS).toISOString() : states.observedA,
    accepts_remote_work: true, harnesses: [], capabilities: []
  });
  seedMachine(api, {
    name: 'runner-b', role: 'runner', parent_machine_id: carrier.id, rank: 5, labels: [],
    availability: states.b ?? 'online', observed_at: null, accepts_remote_work: true, harnesses: [], capabilities: []
  });
}

async function openRunner(page: Page, name: string) {
  await page.locator('main').getByRole('radio', { name: new RegExp(`^${name}`) }).check();
  await expect(page.locator('main').getByRole('heading', { name })).toBeVisible();
}

test('runner_card_names_role_parent_labels_and_rank', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api));
  const main = page.locator('main');
  await openRunner(page, 'runner-a');
  await expect(main.locator('[data-runner-badge]')).toHaveText('Раннер');
  await expect(main.locator('[data-runner-parent]').getByRole('link', { name: 'workstation' })).toBeVisible();
  await expect(main.locator('[data-runner-rank]')).toHaveText('Ранг 20');
  await expect(main.getByRole('list', { name: 'Метки раннера' }).getByRole('listitem')).toHaveText(['build-host', 'kernel-6.12']);
  await expect(main.getByRole('heading', { name: 'Задачи' })).toBeVisible();
  // Сессий на раннере нет: раздела о них на карточке тоже нет.
  await expect(main.getByRole('heading', { name: 'Сессии на машине' })).toHaveCount(0);
  await expect(main.locator('[data-carrier-line]')).toHaveCount(0);
});

test('runner_card_parent_link_opens_the_carrier_card', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api));
  const main = page.locator('main');
  await openRunner(page, 'runner-a');
  await main.locator('[data-runner-parent]').getByRole('link', { name: 'workstation' }).click();
  await expect(main.getByRole('heading', { name: 'workstation' })).toBeVisible();
  await expect(main.locator('[data-runner]')).toHaveCount(0);
});

test('runner_card_says_online_in_words', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api, { a: 'online' }));
  await openRunner(page, 'runner-a');
  const state = page.locator('main [data-runner-availability]');
  await expect(state).toHaveText('на связи');
  await expect(state).toHaveAttribute('data-tone', 'success');
});

test('runner_card_says_offline_with_the_age_of_the_check', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api, { a: 'offline' }));
  await openRunner(page, 'runner-a');
  const state = page.locator('main [data-runner-availability]');
  await expect(state).toHaveText('недоступен · проверка 2 мин назад');
  await expect(state).toHaveAttribute('data-tone', 'danger');
  // Слова несут смысл без цвета и значков: в строке нет ни галочки, ни крестика.
  await expect(state).not.toContainText(/[✓✔✗✕×]/);
});

test('runner_card_says_unknown_when_there_is_no_fresh_check', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api, { a: 'unknown', observedA: null }));
  await openRunner(page, 'runner-a');
  const state = page.locator('main [data-runner-availability]');
  await expect(state).toHaveText('неизвестно · нет свежей проверки');
  await expect(state).toHaveAttribute('data-tone', 'muted');
});

test('runner_availability_follows_the_hub_event_live', async ({ page }) => {
  const { hub, api } = await bootSecondary(page, '/machines', (api) => seedRunners(api, { a: 'online' }));
  await openRunner(page, 'runner-a');
  const runner = [...api.machines.values()].find((machine) => machine.name === 'runner-a')!;
  const state = page.locator('main [data-runner-availability]');
  await expect(state).toHaveText('на связи');
  // Хаб шлёт раннеру свои слова `online|offline|unknown` и возраст проверки несущей: экран берёт их как есть.
  hub.append('machines', { kind: 'machine_availability_changed', payload: { machine_id: runner.id, availability: 'offline', observed_at: new Date(Date.now() - 45_000).toISOString() } }); hub.flush();
  await expect(state).toHaveText('недоступен · проверка меньше минуты назад');
  await expect(state).toHaveAttribute('data-tone', 'danger');
  hub.append('machines', { kind: 'machine_availability_changed', payload: { machine_id: runner.id, availability: 'unknown' } }); hub.flush();
  await expect(state).toHaveText('неизвестно · нет свежей проверки');
  hub.append('machines', { kind: 'machine_availability_changed', payload: { machine_id: runner.id, availability: 'online' } }); hub.flush();
  await expect(state).toHaveText('на связи');
});

test('carrier_card_names_its_runners', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api));
  const line = page.locator('main [data-carrier-line]');
  await expect(line).toHaveText('Несущая раннеров: runner-a, runner-b');
  await line.getByRole('link', { name: 'runner-b' }).click();
  await expect(page.locator('main').getByRole('heading', { name: 'runner-b' })).toBeVisible();
  await expect(page.locator('main [data-runner-rank]')).toHaveText('Ранг 5');
});

test('runner_switch_is_a_pause_and_an_ordinary_machine_keeps_its_wording', async ({ page }) => {
  const { api } = await bootSecondary(page, '/machines', (api) => seedRunners(api));
  const main = page.locator('main');
  await expect(main.getByRole('switch', { name: 'Принимать делегатов и команды с других машин' })).toBeVisible();
  await openRunner(page, 'runner-a');
  const toggle = main.getByRole('switch', { name: 'Принимать задачи' });
  await expect(toggle).toBeChecked();
  await expect(main.locator('[data-remote-work-note]')).toHaveText('Выключите, чтобы поставить раннер на паузу: идущие задачи доработают, новые сюда не попадут');
  await expect(main.getByRole('switch', { name: 'Принимать делегатов и команды с других машин' })).toHaveCount(0);
  await toggle.uncheck();
  await expect(toggle).not.toBeChecked();
  expect(api.remoteWorkCalls).toHaveLength(1);
  expect(api.remoteWorkCalls[0]!.accepts).toBe(false);
});

test('runner_and_carrier_cards_have_no_axe_violations', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api, { a: 'unknown', observedA: null }));
  await expect(page.locator('main [data-carrier-line]')).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await openRunner(page, 'runner-a');
  await expect(page.locator('main [data-runner-availability]')).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});

test('new_session_machine_picker_leaves_runners_out', async ({ page }) => {
  await bootSecondary(page, '/machines', (api) => seedRunners(api));
  await openNewSessionForm(page, { expanded: true });
  const picker = page.getByRole('combobox', { name: 'Машина' });
  await expect(picker.getByRole('option', { name: 'workstation' })).toHaveCount(1);
  await expect(picker.getByRole('option', { name: /runner-/ })).toHaveCount(0);
});
