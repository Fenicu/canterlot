import { shortAge } from './model.js';

/** Поля машины, которыми живёт роль раннера; у снимка старого хаба их нет. */
export interface RoleFields {
  role?: string;
  parent_machine_id?: string | null;
  is_carrier?: boolean;
}

export function isRunner(machine: Pick<RoleFields, 'role'>): boolean {
  return machine.role === 'runner';
}

/** Машины, на которых можно начать сессию: раннеры исполняют только задачи. */
export function sessionMachines<T extends Pick<RoleFields, 'role'>>(machines: readonly T[]): T[] {
  return machines.filter((machine) => !isRunner(machine));
}

export type RunnerTone = 'success' | 'danger' | 'muted';

export interface RunnerAvailability {
  text: string;
  tone: RunnerTone;
}

/**
 * Доступность раннера словами. Возраст проверки берётся от наблюдения несущей, а у `unknown` не
 * показывается: хаб назвал его неизвестным именно потому, что свежей проверки нет.
 */
export function runnerAvailability(machine: { availability: string; observed_at?: string | null }, now: number): RunnerAvailability {
  if (machine.availability === 'online') return { text: 'на связи', tone: 'success' };
  if (machine.availability === 'offline') {
    const observed = machine.observed_at ? Date.parse(machine.observed_at) : Number.NaN;
    if (Number.isNaN(observed)) return { text: 'недоступен', tone: 'danger' };
    return { text: `недоступен · проверка ${shortAge(now - observed)} назад`, tone: 'danger' };
  }
  return { text: 'неизвестно · нет свежей проверки', tone: 'muted' };
}

export interface CarrierLine {
  text: string;
  runners: { id: string; name: string }[];
}

/** Строка несущей: чьих раннеров она ведёт. У машины без метки несущей строки нет. */
export function carrierLine(
  machine: { id: string; is_carrier?: boolean },
  machines: readonly { id: string; name: string; role?: string; parent_machine_id?: string | null }[]
): CarrierLine | null {
  if (!machine.is_carrier) return null;
  const runners = machines
    .filter((item) => isRunner(item) && item.parent_machine_id === machine.id)
    .map((item) => ({ id: item.id, name: item.name }))
    .sort((a, b) => a.name.localeCompare(b.name));
  return { text: `Несущая раннеров: ${runners.length ? runners.map((item) => item.name).join(', ') : 'раннеров пока нет'}`, runners };
}

/** Подпись выключателя удалённой работы: у раннера это пауза, у обычной машины — прежняя настройка. */
export function remoteWorkText(machine: Pick<RoleFields, 'role'>): { label: string; hint: string } {
  if (isRunner(machine)) {
    return {
      label: 'Принимать задачи',
      hint: 'Выключите, чтобы поставить раннер на паузу: идущие задачи доработают, новые сюда не попадут'
    };
  }
  return {
    label: 'Принимать делегатов и команды с других машин',
    hint: 'Выключено: сессии других машин не могут запускать здесь делегатов. Свои сессии машины это не меняет'
  };
}
