import { failureLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import { alarmEventLine, alarmTextCount } from '#lib/timeline/alarm.js';
import type { InputOrigin } from '#lib/stores/inputs.js';

export type HubEvent = NonNullable<InputOrigin['event']>;
export type HubFact = { label: string; value: string };

/** Поля сработавшего будильника: хаб кладёт их в origin рядом с общими полями события. */
type AlarmFacts = {
  alarm_id?: string | null;
  alarm_ids?: string[] | null;
  alarm_key?: string | null;
  fired_reason?: string | null;
};
type AlarmOrigin = InputOrigin & AlarmFacts & { event?: (HubEvent & AlarmFacts) | null };

const SESSION_KINDS = new Set(['session_result', 'asked', 'stalled', 'failed', 'quota_paused']);

const STATUS: Record<string, string> = {
  idle: 'завершена',
  waiting_for_user: 'задала вопрос',
  stalled: 'зависла',
  failed: 'сорвалась',
  quota_paused: 'ждёт лимита',
  stopped: 'остановлена'
};

export const LEGACY_LIMIT = 420;

// Вызывается на каждой записи ленты при каждой пересборке окна: вид читается один раз,
// чтобы поле не подписывало строку дважды.
export function isEventOrigin(origin: InputOrigin | null | undefined): boolean {
  const kind = origin?.kind;
  return kind === 'event' || kind === 'alarm';
}

export function isSessionEvent(event: HubEvent | null | undefined): boolean {
  return event != null && SESSION_KINDS.has(event.kind);
}

export function sessionStatus(event: HubEvent): string {
  return (event.state && STATUS[event.state]) || 'завершена';
}

export function eventTitle(origin: InputOrigin | null | undefined): string {
  return origin?.title?.trim() || 'Сессия';
}

export function eventSessionId(origin: InputOrigin | null | undefined): string | null {
  return origin?.event?.session_id ?? origin?.from ?? null;
}

const LIMIT_WINDOWS: Record<string, string> = {
  five_hour: '5 часов',
  weekly: 'неделя'
};

const PHASES: Record<string, string> = {
  generating: 'генерирует ответ',
  running_tool: 'выполняет инструмент',
  context_maintenance: 'сжимает контекст',
  starting: 'запускается',
  stopping: 'останавливается',
  delivering_attachments: 'передаёт файлы',
  awaiting_permission: 'ждёт разрешения',
  awaiting_answer: 'ждёт ответа',
  no_link: 'нет связи с машиной'
};

function windowLabel(value: string): string {
  return LIMIT_WINDOWS[value] ?? 'другое окно';
}

function phaseLabel(value: string): string {
  return PHASES[value] ?? 'работает';
}

function reasonLabel(value: string): string {
  const label = failureLabel(value);
  return label === 'причина неизвестна' ? value : label;
}

function moment(value: string | null | undefined): string | null {
  if (!value) return null;
  const ts = Date.parse(value);
  return Number.isFinite(ts) ? fmt.resetAt(ts, ts) : null;
}

export function eventFacts(event: HubEvent): HubFact[] {
  const facts: HubFact[] = [];
  if (event.turns != null) facts.push({ label: 'ходы', value: String(event.turns) });
  if (event.duration_ms != null) {
    if (event.working_ms != null)
      facts.push({ label: 'работала', value: fmt.worked(event.working_ms, event.duration_ms) });
    else facts.push({ label: 'время', value: fmt.duration(event.duration_ms) });
  } else if (event.working_ms != null)
    facts.push({ label: 'работала', value: fmt.durationCoarse(event.working_ms) });
  if (event.commits != null)
    facts.push({ label: 'коммиты', value: String(event.commits.length) });
  if (event.changed_files != null)
    facts.push({ label: 'файлы', value: String(event.changed_files) });
  if (event.question) facts.push({ label: 'вопрос', value: event.question });
  if (event.reason) facts.push({ label: 'причина', value: reasonLabel(event.reason) });
  if (event.provider)
    facts.push({ label: 'провайдер', value: providerLabel(event.provider) });
  if (event.window)
    facts.push({ label: 'окно лимита', value: windowLabel(event.window) });
  const until = moment(event.until);
  if (until) facts.push({ label: 'лимит до', value: until });
  if (event.phase)
    facts.push({ label: 'последнее действие', value: phaseLabel(event.phase) });
  const last = moment(event.last_event_at);
  if (last) facts.push({ label: 'последнее событие', value: last });
  return facts;
}

function quoted(title: string): string {
  return title ? `«${title}»` : 'Сессия';
}

function branchClause(branch: string | null | undefined, kept: boolean): string {
  return branch ? `, ветка ${branch} ${kept ? 'оставлена' : 'удалена'}` : '';
}

/** Тихая строка для мелкого события: русский текст строится только из фактов. */
export function eventLine(event: HubEvent, title: string): string | null {
  const name = quoted(title);
  switch (event.kind) {
    case 'accepted': {
      if (event.closed) return `${name} принята родителем, события больше не придут`;
      switch (event.worktree) {
        case 'removed':
          return event.branch_kept != null
            ? `${name} принята, рабочая копия убрана${branchClause(event.branch, event.branch_kept)}`
            : `${name} принята, рабочая копия убрана`;
        case 'none':
          return `${name} принята, сессия остановлена и в архиве`;
        case 'dirty':
          return `${name} остановлена, в рабочей копии остались правки`;
        case 'failed':
          return `${name} остановлена, рабочую копию убрать не удалось`;
        default:
          return `${name} принята`;
      }
    }
    case 'stopped':
      return `${name} остановлена`;
    case 'approval':
      switch (event.decision) {
        case 'allowed':
          return title ? `Запуск «${title}» разрешён` : 'Запуск сессии разрешён';
        case 'denied':
          return 'Запуск сессии отклонён владельцем';
        case 'failed':
          return 'Запуск разрешён, но сессия не стартовала';
        case 'lapsed':
          return 'Запрос на запуск истёк';
        default:
          return null;
      }
    case 'deleted':
      return title ? `${name} удалена` : 'Сессия удалена';
    case 'alarm':
      return alarmEventLine(event);
    default:
      return null;
  }
}

function stringValues(value: unknown): string[] {
  if (Array.isArray(value))
    return value.filter((item): item is string => typeof item === 'string' && item.trim() !== '');
  return typeof value === 'string' && value.trim() !== '' ? [value] : [];
}

/** Идентификаторы сработавших будильников: новые клиенты получают их списком, старые — по одному. */
export function alarmIdentifiers(origin: InputOrigin | null | undefined): string[] {
  const value = origin as AlarmOrigin | null | undefined;
  return [
    ...new Set([
      ...stringValues(value?.alarm_ids),
      ...stringValues(value?.event?.alarm_ids),
      ...stringValues(value?.alarm_id),
      ...stringValues(value?.event?.alarm_id)
    ])
  ];
}

/** Сколько будильников сработало: список в origin или разделы доставленного текста. */
export function alarmFiredCount(origin: InputOrigin | null | undefined, text: string): number {
  return Math.max(alarmIdentifiers(origin).length, alarmTextCount(text));
}

/** Строка на ввод: слабая ссылка не держит ушедшие записи. */
const alarmLines = new WeakMap<object, { key: string; text: string; line: string }>();

/** Всё, от чего зависит строка: тот же ввод с тем же текстом не разбирается заново. */
function alarmLineKey(origin: InputOrigin): string {
  const event = (origin as AlarmOrigin).event;
  return [...alarmIdentifiers(origin), event?.alarm_key ?? '', event?.fired_reason ?? ''].join(
    '\u0000'
  );
}

/** Тихая строка-заголовок будильника: имя одного или число сработавших. */
export function alarmEntryLine(origin: InputOrigin | null | undefined, text: string): string | null {
  if (origin?.kind !== 'alarm') return null;
  // Текст доставки разбирается один раз на ввод: пересборка окна и снимок не гоняют регулярки заново.
  const key = alarmLineKey(origin);
  const known = alarmLines.get(origin);
  if (known && known.key === key && known.text === text) return known.line;
  const line = alarmTitleLine(origin, text);
  alarmLines.set(origin, { key, text, line });
  return line;
}

function alarmTitleLine(origin: InputOrigin, text: string): string {
  const event = (origin as AlarmOrigin).event;
  const count = alarmFiredCount(origin, text);
  if (count > 1)
    return `${fmt.plural(count, ['Сработал', 'Сработали', 'Сработали'])} ${count} ${fmt.plural(
      count,
      ['будильник', 'будильника', 'будильников']
    )}`;
  return alarmEventLine({ alarm_key: event?.alarm_key, fired_reason: event?.fired_reason });
}

export function isLongText(text: string): boolean {
  return text.length > LEGACY_LIMIT;
}

export function collapsedText(text: string): string {
  return isLongText(text) ? `${text.slice(0, LEGACY_LIMIT)}…` : text;
}

export const LEGACY_CAPTION = 'Сообщение хаба';
