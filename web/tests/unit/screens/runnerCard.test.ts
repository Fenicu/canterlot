import { describe, expect, it } from 'vitest';
import { carrierLine, isRunner, remoteWorkText, runnerAvailability, sessionMachines } from '../../../src/lib/screens/machines/runner';

const NOW = Date.parse('2026-11-21T12:00:00.000Z');
const ago = (ms: number) => new Date(NOW - ms).toISOString();

describe('runnerAvailability', () => {
  it('names a connected runner without an age', () => {
    expect(runnerAvailability({ availability: 'online', observed_at: ago(5_000) }, NOW)).toEqual({ text: 'на связи', tone: 'success' });
  });

  it('names an offline runner with the age of the last check', () => {
    expect(runnerAvailability({ availability: 'offline', observed_at: ago(2 * 60_000 + 10_000) }, NOW)).toEqual({ text: 'недоступен · проверка 2 мин назад', tone: 'danger' });
  });

  it('rounds a check younger than a minute to words', () => {
    expect(runnerAvailability({ availability: 'offline', observed_at: ago(20_000) }, NOW).text).toBe('недоступен · проверка меньше минуты назад');
  });

  it('does not invent an age when the hub has no observation', () => {
    expect(runnerAvailability({ availability: 'offline', observed_at: null }, NOW).text).toBe('недоступен');
  });

  it('says that nothing fresh is known, whatever the old observation says', () => {
    expect(runnerAvailability({ availability: 'unknown', observed_at: ago(40 * 60_000) }, NOW)).toEqual({ text: 'неизвестно · нет свежей проверки', tone: 'muted' });
    expect(runnerAvailability({ availability: 'unknown', observed_at: null }, NOW).text).toBe('неизвестно · нет свежей проверки');
  });

  it('treats a value the PWA does not know as unknown', () => {
    expect(runnerAvailability({ availability: 'available', observed_at: null }, NOW).tone).toBe('muted');
  });

  it('shows hours for an old check', () => {
    expect(runnerAvailability({ availability: 'offline', observed_at: ago(3 * 3_600_000) }, NOW).text).toBe('недоступен · проверка 3 ч назад');
  });
});

describe('machine roles', () => {
  it('tells a runner from a machine, with the role missing on old snapshots', () => {
    expect(isRunner({ role: 'runner' })).toBe(true);
    expect(isRunner({ role: 'worker' })).toBe(false);
    expect(isRunner({})).toBe(false);
  });

  it('leaves runners out of the machines that start sessions', () => {
    const items = [{ id: 'a', role: 'worker' }, { id: 'b', role: 'runner' }, { id: 'c' }];
    expect(sessionMachines(items).map((m) => m.id)).toEqual(['a', 'c']);
  });
});

describe('carrierLine', () => {
  const runners = [
    { id: 'r2', name: 'runner-b', role: 'runner', parent_machine_id: 'vm' },
    { id: 'r1', name: 'runner-a', role: 'runner', parent_machine_id: 'vm' },
    { id: 'r3', name: 'other', role: 'runner', parent_machine_id: 'else' },
    { id: 'vm', name: 'carrier', role: 'worker', parent_machine_id: null }
  ];

  it('names the runners of the carrier in name order', () => {
    expect(carrierLine({ id: 'vm', is_carrier: true }, runners)).toEqual({ text: 'Несущая раннеров: runner-a, runner-b', runners: [{ id: 'r1', name: 'runner-a' }, { id: 'r2', name: 'runner-b' }] });
  });

  it('says so when a carrier has no runners yet', () => {
    expect(carrierLine({ id: 'vm', is_carrier: true }, [])?.text).toBe('Несущая раннеров: раннеров пока нет');
  });

  it('is absent on a machine that is not a carrier', () => {
    expect(carrierLine({ id: 'vm', is_carrier: false }, runners)).toBeNull();
    expect(carrierLine({ id: 'vm' }, runners)).toBeNull();
  });
});

describe('remoteWorkText', () => {
  it('keeps today’s wording for an ordinary machine', () => {
    expect(remoteWorkText({ role: 'worker' }).label).toBe('Принимать делегатов и команды с других машин');
  });

  it('turns the switch into a pause for a runner', () => {
    const text = remoteWorkText({ role: 'runner' });
    expect(text.label).toBe('Принимать задачи');
    expect(text.hint).toBe('Выключите, чтобы поставить раннер на паузу: идущие задачи доработают, новые сюда не попадут');
  });
});
