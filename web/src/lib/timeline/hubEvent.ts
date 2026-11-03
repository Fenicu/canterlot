import { failureLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import { alarmEventLine } from '#lib/timeline/alarm.js';
import type { InputOrigin } from '#lib/stores/inputs.js';

export type HubEvent = NonNullable<InputOrigin['event']>;
export type HubFact = { label: string; value: string };

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

export function isEventOrigin(origin: InputOrigin | null | undefined): boolean {
  return origin?.kind === 'event' || origin?.kind === 'alarm';
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

export function isLongText(text: string): boolean {
  return text.length > LEGACY_LIMIT;
}

export function collapsedText(text: string): string {
  return isLongText(text) ? `${text.slice(0, LEGACY_LIMIT)}…` : text;
}

export const LEGACY_CAPTION = 'Сообщение хаба';
