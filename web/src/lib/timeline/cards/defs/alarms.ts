// Будильники агента: взвод, отмена и список. Слова — как у плашки будильников над вводом.
import { fmt } from '#lib/format/ru.js';
import type { AlarmView } from '#lib/stores/chat.svelte.js';
import { alarmChip } from '#lib/timeline/alarm.js';
import { bool, isObject, list, num, obj, str, strings, type JsonObject } from '../json';
import { hubError } from '../copy';
import type { CardCall, CardLogic } from '../types';

const SETTLE_VERBS: Record<string, [string, string]> = {
  idle: ['закончит', 'закончат'],
  waiting_for_user: ['спросит', 'спросят'],
  stalled: ['зависнет', 'зависнут'],
  quota_paused: ['встанет на лимит', 'встанут на лимит'],
  failed: ['сорвётся', 'сорвутся'],
  stopped: ['остановится', 'остановятся'],
  deleted: ['удалится', 'удалятся']
};

function moment(value: string | null | undefined): string {
  const ts = value ? Date.parse(value) : Number.NaN;
  return Number.isFinite(ts) ? fmt.clock(ts) : '';
}

/** Вид будильника из входа вызова и ответа хаба: те же поля, что у плашки над вводом. */
export function alarmFromCall(input: JsonObject, answer: JsonObject | null, now: number): AlarmView {
  const when = obj(input.when);
  const kind = isObject(input.when) ? 'condition' : 'at';
  const inSeconds = num(input.in_seconds);
  const firesAt = str(answer?.fires_at) || str(input.at) || (inSeconds !== null ? new Date(now + inSeconds * 1000).toISOString() : '');
  const deadline = num(input.deadline_seconds);
  return {
    alarm_id: str(answer?.alarm_id),
    batch: bool(input.batch) === true,
    below_percent: num(when.below_percent),
    condition: str(answer?.condition),
    conversation_id: '',
    created_at: new Date(now).toISOString(),
    deadline_at: str(answer?.deadline_at) || (kind === 'condition' && deadline !== null ? new Date(now + deadline * 1000).toISOString() : null),
    fires_at: kind === 'at' ? firesAt || null : null,
    free_disk_gb_above: num(when.free_disk_gb_above),
    fresh: bool(when.fresh),
    key: str(input.key) || null,
    kind,
    load_per_core_below: num(when.load_per_core_below),
    mem_available_gb_above: num(when.mem_available_gb_above),
    min_bytes: num(when.min_bytes),
    note: str(input.note) || null,
    note_bytes: str(input.note).length,
    only_on_timeout: bool(input.only_on_timeout) === true,
    path: str(when.path) || null,
    provider: str(when.provider) || null,
    read_head: bool(when.read_head),
    session_ids: strings(when.session_ids),
    state: str(answer?.state) || 'armed',
    states: strings(when.states).length > 0 ? strings(when.states) : str(when.kind) === 'session_state' ? ['idle', 'waiting_for_user', 'failed', 'stopped'] : [],
    window: str(when.window) || null
  } as AlarmView;
}

function joinRu(items: string[]): string {
  if (items.length <= 1) return items[0] ?? '';
  return `${items.slice(0, -1).join(', ')} и ${items.at(-1)}`;
}

/**
 * Строка будильника на сессии с их заголовками: «Будильник · когда закончат «T5» и «T6» · до 18:00».
 * Остальные условия говорит плашка над вводом, `alarmChip`.
 */
export function alarmLine(alarm: AlarmView, title: (id: string) => string): string {
  if (alarm.session_ids.length === 0 || alarm.only_on_timeout || alarm.state === 'failed') return alarmChip(alarm);
  const many = alarm.session_ids.length > 1;
  const verbs = [...new Set(alarm.states.map((state) => SETTLE_VERBS[state]?.[many ? 1 : 0]).filter(Boolean))];
  const verb = verbs.length > 0 ? verbs.slice(0, 2).join(' или ') : many ? 'изменятся' : 'изменится';
  const who = joinRu(alarm.session_ids.map((id) => `«${title(id)}»`));
  const name = alarm.key ? `«${alarm.key}» · ` : '';
  const until = moment(alarm.deadline_at);
  return `Будильник · ${name}когда ${who} ${verb}${until ? ` · до ${until}` : ''}`;
}

function titleOf(call: CardCall): (id: string) => string {
  return (id) => call.session(id)?.title || id.slice(0, 8);
}

export interface AlarmSetData {
  alarm: AlarmView;
  /** Условие уже выполнено: хаб ничего не взвёл. */
  satisfied: boolean;
  error: string;
}

export const alarmSet: CardLogic<AlarmSetData> = {
  stays: true,
  label: 'Будильник',
  parse(call) {
    if (!call.hasInput && !call.answer && call.state !== 'error') return null;
    const now = call.block.createdAt || Date.now();
    const alarm = alarmFromCall(call.input, call.answer, now);
    return { alarm, satisfied: alarm.state === 'satisfied', error: call.state === 'error' ? hubError(call.output) : '' };
  },
  summary(call, data) {
    if (data.error) return `Будильник не взведён: ${data.error}`;
    return alarmLine(data.alarm, titleOf(call));
  }
};

export interface AlarmCancelData {
  key: string;
  result: string;
  error: string;
}

export const alarmCancel: CardLogic<AlarmCancelData> = {
  label: 'Будильник',
  parse(call) {
    const alarm = obj(call.answer?.alarm);
    return {
      key: str(call.input.key) || str(alarm.key),
      result: str(call.answer?.result),
      error: call.state === 'error' ? hubError(call.output) : ''
    };
  },
  summary(_call, data) {
    const name = data.key ? ` «${data.key}»` : '';
    if (data.error) return `Будильник${name} не отменён: ${data.error}`;
    if (data.result === 'already_fired') return `Будильник${name} уже сработал`;
    if (data.result === 'already_cancelled') return `Будильник${name} уже отменён`;
    return `Будильник${name} отменён`;
  }
};

export interface AlarmListData {
  alarms: AlarmView[];
  /** Ближайшее время срабатывания или срока среди взведённых. */
  next: number | null;
}

export const alarmList: CardLogic<AlarmListData> = {
  label: 'Будильники',
  parse(call) {
    if (!call.answer) return call.state === 'running' ? { alarms: [], next: null } : null;
    const alarms = list(call.answer.alarms).filter(isObject).map((row) => ({ ...row, session_ids: strings(row.session_ids), states: strings(row.states) }) as unknown as AlarmView);
    const times = alarms
      .filter((alarm) => alarm.state === 'armed')
      .map((alarm) => Date.parse(alarm.fires_at ?? alarm.deadline_at ?? ''))
      .filter(Number.isFinite);
    return { alarms, next: times.length > 0 ? Math.min(...times) : null };
  },
  summary(_call, data) {
    const armed = data.alarms.filter((alarm) => alarm.state === 'armed').length;
    if (data.alarms.length === 0) return 'Будильники: ни одного';
    const next = data.next !== null ? ` · ближайший в ${fmt.clock(data.next)}` : '';
    return `Будильники: ${armed} ${fmt.plural(armed, ['взведён', 'взведены', 'взведено'])}${next}`;
  }
};

export function alarmListLine(call: CardCall, alarm: AlarmView): string {
  return alarmLine(alarm, titleOf(call));
}
