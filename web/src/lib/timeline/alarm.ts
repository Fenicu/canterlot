import { providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import type { AlarmView } from '#lib/stores/chat.svelte.js';

const STATE_VERBS: Record<string, string> = {
  idle: 'закончится',
  waiting_for_user: 'спросит',
  stalled: 'зависнет',
  quota_paused: 'встанет на лимит',
  failed: 'сорвётся',
  stopped: 'остановится',
  deleted: 'удалится'
};

function moment(value: string | null | undefined): string {
  const ts = value ? Date.parse(value) : NaN;
  return Number.isFinite(ts) ? fmt.clock(ts) : '';
}

function target(id: string): string {
  return id.slice(0, 8);
}

function targets(ids: string[]): string {
  return ids.length ? ids.map(target).join(', ') : 'сессия';
}

function verbs(states: string[]): string {
  const known = states.map((state) => STATE_VERBS[state]).filter(Boolean);
  return known.length ? [...new Set(known)].join(' или ') : 'изменится';
}

const LIMIT_WINDOWS: Record<string, string> = {
  five_hour: '5 ч',
  weekly: 'неделя',
  monthly: 'месяц'
};

function limitText(alarm: AlarmView): string {
  const window = alarm.window ? (LIMIT_WINDOWS[alarm.window] ?? 'окно') : 'окно';
  const base = `Сброс лимита ${providerLabel(alarm.provider)}, окно ${window}`;
  return alarm.below_percent != null
    ? `${base} или расход не выше ${fmt.number(alarm.below_percent)}%`
    : base;
}

function machineText(alarm: AlarmView): string {
  const parts: string[] = [];
  if (alarm.free_disk_gb_above != null)
    parts.push(`свободно на диске больше ${fmt.number(alarm.free_disk_gb_above)} ГБ`);
  if (alarm.load_per_core_below != null)
    parts.push(`нагрузка на ядро ниже ${fmt.number(alarm.load_per_core_below)}`);
  if (alarm.mem_available_gb_above != null)
    parts.push(`память больше ${fmt.number(alarm.mem_available_gb_above)} ГБ`);
  return parts.length ? `Машина: ${parts.join(', ')}` : 'Машина';
}

function structured(alarm: AlarmView): string | null {
  if (alarm.provider != null) return limitText(alarm);
  if (
    alarm.free_disk_gb_above != null ||
    alarm.load_per_core_below != null ||
    alarm.mem_available_gb_above != null
  )
    return machineText(alarm);
  return null;
}

/** Плашка под лентой: время, условие или сторож. */
export function alarmChip(alarm: AlarmView): string {
  if (alarm.state === 'failed') return 'Будильник · не доставлен';
  const condition = structured(alarm);
  if (alarm.only_on_timeout) {
    const watched = alarm.session_ids.length
      ? targets(alarm.session_ids)
      : condition || alarm.condition || 'условие';
    return `Сторож · ${watched} · до ${moment(alarm.deadline_at)}`;
  }
  if (alarm.kind === 'at') {
    const at = moment(alarm.fires_at);
    return at ? `Будильник · ${at}` : 'Будильник';
  }
  const name = alarm.key ? `«${alarm.key}» · ` : '';
  // Условия лимита и машины собираются из полей, чтобы не показывать английский текст хаба.
  if (condition) return `Будильник · ${name}${condition}`;
  if (!alarm.session_ids.length && alarm.condition) {
    return `Будильник · ${name}${alarm.condition}`;
  }
  return `Будильник · ${name}когда ${targets(alarm.session_ids)} ${verbs(alarm.states)} · до ${moment(alarm.deadline_at)}`;
}

function nearestMoment(alarm: AlarmView): number | null {
  const times = [alarm.fires_at, alarm.deadline_at]
    .map((value) => (value ? Date.parse(value) : NaN))
    .filter((value): value is number => Number.isFinite(value));
  return times.length ? Math.min(...times) : null;
}

/**
 * Свёрнутая полоса: сколько будильников и когда ближайший из них. Сторожа
 * считаются будильниками, потому что для владельца это одно и то же действие.
 */
export function alarmSummary(alarms: AlarmView[]): string {
  const count = alarms.length;
  // «Будильники · 4» — заголовок строки, поэтому форма зависит от числа.
  const label = fmt.plural(count, ['Будильник', 'Будильники', 'Будильники']);
  const nearest = alarms
    .map(nearestMoment)
    .filter((value): value is number => value !== null)
    .sort((a, b) => a - b)[0];
  return nearest === undefined ? `${label} · ${count}` : `${label} · ${count} · ближайший ${fmt.clock(nearest)}`;
}

/** Что показывает раскрытая плашка: условие, заметку и причину сбоя. */
export function alarmDetail(alarm: AlarmView): string[] {
  const lines: string[] = [];
  const condition = structured(alarm);
  if (alarm.kind === 'at') lines.push(`Время: ${moment(alarm.fires_at)}`);
  else if (alarm.session_ids.length) lines.push(`Состояния: ${verbs(alarm.states)}`);
  else if (condition) lines.push(`Условие: ${condition}`);
  else if (alarm.condition) lines.push(`Условие: ${alarm.condition}`);
  if (alarm.fired_reason === 'timed_out') lines.push('Срок условия истёк');
  if (alarm.failure_reason === 'delivery_failed') lines.push('Не удалось доставить сообщение');
  if (alarm.note) lines.push(`Заметка: ${alarm.note}`);
  return lines;
}

/** Служебная строка ленты для сработавшего будильника. */
export function alarmEventLine(event: {
  alarm_key?: string | null;
  fired_reason?: string | null;
}): string {
  const name = event.alarm_key ? `«${event.alarm_key}»` : '';
  switch (event.fired_reason) {
    case 'timed_out':
      return `Будильник${name ? ` ${name}` : ''}: истёк срок условия`;
    case 'condition':
      return `Будильник${name ? ` ${name}` : ''}: условие выполнено`;
    case 'by_clock':
      return `Будильник${name ? ` ${name}` : ''}: лимит, вероятно, сброшен (по часам, без подтверждения)`;
    default:
      return `Будильник${name ? ` ${name}` : ''}: сработал`;
  }
}

/**
 * Сколько будильников описано в доставленном тексте: заголовок пачки
 * («3 alarms fired») или отдельные строки-разделы. Итоги сессий внутри
 * пачки начинаются с `[canterlot] Session`, поэтому за будильники не считаются.
 */
export function alarmTextCount(text: string): number {
  let batch = 0;
  for (const match of text.matchAll(/\[canterlot\]\s*(\d+)\s+alarms?\s+fired\b/gi))
    batch = Math.max(batch, Number(match[1]));
  const sections = text.match(/^[ \t]*(?:-[ \t]+)?\[canterlot\]\s+Alarm\b/gim)?.length ?? 0;
  return Math.max(batch, sections);
}
