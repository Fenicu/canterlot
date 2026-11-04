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

/** Плашка под лентой: время, условие или сторож. */
export function alarmChip(alarm: AlarmView): string {
  if (alarm.state === 'failed') return 'Будильник · не доставлен';
  if (alarm.only_on_timeout) {
    return `Сторож · ${targets(alarm.session_ids)} · до ${moment(alarm.deadline_at)}`;
  }
  if (alarm.kind === 'at') {
    const at = moment(alarm.fires_at);
    return at ? `Будильник · ${at}` : 'Будильник';
  }
  const name = alarm.key ? `«${alarm.key}» · ` : '';
  return `Будильник · ${name}когда ${targets(alarm.session_ids)} ${verbs(alarm.states)} · до ${moment(alarm.deadline_at)}`;
}

/** Что показывает раскрытая плашка: условие, заметку и причину сбоя. */
export function alarmDetail(alarm: AlarmView): string[] {
  const lines: string[] = [];
  if (alarm.kind === 'at') lines.push(`Время: ${moment(alarm.fires_at)}`);
  else lines.push(`Состояния: ${verbs(alarm.states)}`);
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
