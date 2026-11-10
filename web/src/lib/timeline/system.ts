import { modelLabel, providerLabel } from '#lib/format/labels.js';
import { fmt } from '#lib/format/ru.js';
import { SESSION_STATE_WORD, windowLabel } from '#lib/timeline/hubEvent.js';
import type { InputOrigin } from '#lib/stores/inputs.js';
import type { IconName } from '#lib/ui/icons.js';

export type SystemTone = 'neutral' | 'warning';
export type SystemFact = { label: string; value: string };
export type SystemRow = { title: string; value?: string; href?: string };
export type SystemAction =
  | { kind: 'link'; label: string; href: string }
  | { kind: 'compact'; label: string };

/** Общий вид служебного пузыря: заголовок, причина, факты, строки и свёрнутый текст. */
export type SystemView = {
  kind: 'alarm' | 'machine' | 'context' | 'limit_resumed' | 'quota_paused' | 'quota_note';
  icon: IconName;
  title: string;
  titleHref?: string;
  tone: SystemTone;
  reason?: string;
  facts: SystemFact[];
  rows: SystemRow[];
  note?: string;
  action?: SystemAction;
  /** Полный текст, который получил агент. */
  text: string;
};

/** Ввод другой сессии: бриф запуска или сообщение; рисуется на стороне агента. */
export type SessionMessageView = {
  kind: 'message' | 'spawn';
  title: string;
  model: string | null;
  href: string | null;
  body: string;
  /** Бриф запуска длиннее двенадцати строк: сначала показывается начало. */
  folded: string | null;
};

type CompactCondition = {
  kind?: string;
  at?: string | null;
  session_ids?: string[] | null;
  states?: string[] | null;
  all?: boolean | null;
  provider?: string | null;
  window?: string | null;
  path?: string | null;
};

type AlarmSession = { id: string; title?: string | null; state?: string | null };

type AlarmFacts = {
  alarm_id?: string | null;
  alarm_key?: string | null;
  fired_reason?: string | null;
  alarm_path?: string | null;
  alarm_size?: number | null;
  armed_at?: string | null;
  condition?: unknown;
  note?: string | null;
  sessions?: AlarmSession[] | null;
  alarms?: AlarmFacts[] | null;
};

type MachineDetails = {
  free_gb?: number | null;
  free_pct?: number | null;
  hotspots?: {
    path?: string | null;
    bytes?: number | null;
    incremental_bytes?: number | null;
    newest_at?: string | null;
    title?: string | null;
  }[] | null;
  available_bytes?: number | null;
  total_bytes?: number | null;
  heaviest?: { title?: string | null; execution_id?: string | null; rss_bytes?: number | null }[] | null;
  leaked?: { comm?: string | null; title?: string | null; execution_id?: string | null; rss_bytes?: number | null }[] | null;
  lease_kind?: string | null;
  holder?: string | null;
  holder_title?: string | null;
  conversation_id?: string | null;
  execution_id?: string | null;
  held_minutes?: number | null;
};

const MACHINE_TITLES: Record<string, string> = {
  machine_disk_low: 'мало места',
  machine_disk_critical: 'критически мало места',
  machine_mem_low: 'мало памяти',
  machine_processes_leaked: 'остались процессы',
  lease_long_hold: 'аренда занята долго'
};

function stamp(value: string | null | undefined): number | null {
  if (!value) return null;
  const ts = Date.parse(value);
  return Number.isFinite(ts) ? ts : null;
}

function finite(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function plural(count: number, forms: [string, string, string]): string {
  return `${count} ${fmt.plural(count, forms)}`;
}

function alarmName(key: string | null | undefined): string {
  return key ? `Будильник «${key}»` : 'Будильник';
}

/** Причина срабатывания по структуре; старый ввод без неё сюда не попадает. */
function alarmReason(alarm: AlarmFacts): { reason: string; tone: SystemTone } {
  if (alarm.fired_reason === 'timed_out')
    return { reason: 'Истёк срок ожидания', tone: 'warning' };
  const condition = (alarm.condition ?? {}) as CompactCondition;
  if (condition.kind === 'at') return { reason: 'Пора', tone: 'neutral' };
  if (condition.kind === 'file' && alarm.alarm_path) {
    const size = alarm.alarm_size != null ? ` (${fmt.bytes(alarm.alarm_size)})` : '';
    return { reason: `Появился файл ${alarm.alarm_path}${size}`, tone: 'neutral' };
  }
  if (condition.kind === 'limit_reset') return { reason: 'Лимит сброшен', tone: 'neutral' };
  if (condition.kind === 'session_state' || condition.kind === 'machine' || condition.kind === 'file')
    return { reason: 'Условие выполнено', tone: 'neutral' };
  switch (alarm.fired_reason) {
    case 'condition':
      return { reason: 'Условие выполнено', tone: 'neutral' };
    case 'by_clock':
      return { reason: 'Лимит сброшен', tone: 'neutral' };
    default:
      return { reason: 'Сработал', tone: 'neutral' };
  }
}

function alarmFacts(alarm: AlarmFacts, createdAt: number | null): SystemFact[] {
  const facts: SystemFact[] = [];
  const armed = stamp(alarm.armed_at);
  if (armed !== null) {
    const waited =
      createdAt !== null && createdAt > armed
        ? ` · ждал ${fmt.durationCoarse(createdAt - armed)}`
        : '';
    facts.push({ label: 'Поставлен', value: `${fmt.clock(armed)}${waited}` });
  }
  return facts;
}

function alarmSessionRows(alarm: AlarmFacts): SystemRow[] {
  return (alarm.sessions ?? []).map((session) => ({
    title: session.title?.trim() || 'Сессия',
    value: (session.state && SESSION_STATE_WORD[session.state]) || session.state || '',
    href: `/c/${session.id}`
  }));
}

/** Сработавший будильник: одиночный или пачка. Старый ввод без новых полей — `null`. */
export function alarmView(
  origin: InputOrigin | null | undefined,
  text: string,
  createdAt: number | null
): SystemView | null {
  if (origin?.kind !== 'alarm') return null;
  const event = origin.event as (InputOrigin['event'] & AlarmFacts) | null | undefined;
  if (!event) return null;
  const batch =
    Array.isArray(event.alarms) && event.alarms.length > 1
      ? (event.alarms as AlarmFacts[])
      : null;
  if (batch) {
    const rows = batch.map((alarm) => ({
      title: alarmName(alarm.alarm_key),
      value: alarmReason(alarm).reason
    }));
    return {
      kind: 'alarm',
      icon: 'alarm',
      title: `Сработало ${plural(batch.length, ['будильник', 'будильника', 'будильников'])}`,
      tone: batch.some((alarm) => alarmReason(alarm).tone === 'warning') ? 'warning' : 'neutral',
      facts: [],
      rows,
      text
    };
  }
  if (!event.armed_at && !event.condition) return null;
  const reason = alarmReason(event);
  const note = event.note?.trim() || undefined;
  return {
    kind: 'alarm',
    icon: 'alarm',
    title: alarmName(event.alarm_key),
    tone: reason.tone,
    reason: reason.reason,
    facts: alarmFacts(event, createdAt),
    rows: alarmSessionRows(event),
    note,
    text
  };
}

/** Предупреждение сторожа машины: числа уже в `details`, английский текст не читается. */
export function machineView(
  origin: InputOrigin | null | undefined,
  text: string
): SystemView | null {
  const event = origin?.event;
  const kind = event?.kind;
  if (!event || !kind || !(kind in MACHINE_TITLES)) return null;
  const details = event.details as MachineDetails | null | undefined;
  if (!details) return null;
  const name = event.machine_name?.trim();
  const facts: SystemFact[] = [];
  const rows: SystemRow[] = [];
  let tone: SystemTone = 'neutral';
  if (kind === 'machine_disk_low' || kind === 'machine_disk_critical') {
    const free = finite(details.free_gb);
    const pct = finite(details.free_pct);
    if (free !== null || pct !== null) {
      // Хаб отдаёт округлённые гигабайты; в пузыре все размеры — двоичные, как у `fmt.bytes`.
      const parts = [free !== null ? fmt.bytes(free * 1e9) : '', pct !== null ? `(${fmt.number(pct)} %)` : ''];
      facts.push({ label: 'Свободно', value: parts.filter(Boolean).join(' ') });
    }
    for (const spot of (details.hotspots ?? []).slice(0, 5)) {
      const path = spot.path?.trim();
      if (!path) continue;
      rows.push({
        title: spot.title?.trim() ? `worktree «${spot.title.trim()}» ${path}` : path,
        value: finite(spot.bytes) !== null ? fmt.bytes(spot.bytes as number) : ''
      });
    }
    if (kind === 'machine_disk_critical') tone = 'warning';
  } else if (kind === 'machine_mem_low') {
    const available = finite(details.available_bytes);
    const total = finite(details.total_bytes);
    if (available !== null && total !== null)
      facts.push({ label: 'Память', value: `${fmt.bytes(available)} из ${fmt.bytes(total)}` });
    for (const entry of (details.heaviest ?? []).slice(0, 5)) {
      const title = entry.title?.trim() || `execution ${entry.execution_id ?? ''}`.trim();
      if (title)
        rows.push({
          title,
          value: finite(entry.rss_bytes) !== null ? fmt.bytes(entry.rss_bytes as number) : ''
        });
    }
    tone = 'warning';
  } else if (kind === 'machine_processes_leaked') {
    for (const entry of (details.leaked ?? []).slice(0, 5)) {
      const title = entry.title?.trim()
        ? `${entry.comm ?? 'процесс'} · «${entry.title.trim()}»`
        : (entry.comm ?? 'процесс');
      rows.push({
        title,
        value: finite(entry.rss_bytes) !== null ? fmt.bytes(entry.rss_bytes as number) : ''
      });
    }
  } else {
    const holder =
      details.holder === 'owner'
        ? 'терминал владельца'
        : details.holder_title?.trim()
          ? `сессия «${details.holder_title.trim()}»`
          : details.execution_id
            ? `execution ${details.execution_id}`
            : '';
    if (holder) facts.push({ label: 'Держатель', value: holder });
    if (details.lease_kind)
      facts.push({ label: 'Аренда', value: details.lease_kind === 'gate' ? 'gate' : 'build' });
    const held = finite(details.held_minutes);
    if (held !== null) facts.push({ label: 'Держит', value: `${fmt.number(held)} мин` });
  }
  return {
    kind: 'machine',
    icon: 'server',
    title: name ? `Машина ${name}: ${MACHINE_TITLES[kind]}` : `Машина: ${MACHINE_TITLES[kind]}`,
    tone,
    facts,
    rows,
    action: { kind: 'link', label: 'Открыть машины', href: '/machines' },
    text
  };
}

/** Заполнение контекста: действие сжатия доступно только своей сессии. */
export function contextView(
  origin: InputOrigin | null | undefined,
  text: string,
  conversationId: string | null
): SystemView | null {
  const event = origin?.event;
  if (event?.kind !== 'context_high') return null;
  const percent = Number.parseInt(event.detail ?? '', 10);
  if (!Number.isFinite(percent)) return null;
  const own = conversationId !== null && event.session_id === conversationId;
  return {
    kind: 'context',
    icon: 'gauge',
    title: `Контекст заполнен на ${percent} %`,
    tone: percent >= 90 ? 'warning' : 'neutral',
    facts: [],
    rows: [],
    action: own ? { kind: 'compact', label: 'Сжать контекст' } : undefined,
    text
  };
}

function providerName(value: string | null | undefined): string {
  return value ? providerLabel(value) : '';
}

/** Сброс лимита, пауза делегата и заметка о застрявшем делегате. */
export function limitView(
  origin: InputOrigin | null | undefined,
  text: string
): SystemView | null {
  const event = origin?.event;
  if (!event) return null;
  if (event.kind === 'limit_resumed') {
    const provider = providerName(event.provider);
    const facts: SystemFact[] = [];
    if (event.window) facts.push({ label: 'Окно', value: windowLabel(event.window) });
    const reset = stamp(event.resets_at ?? event.until);
    if (reset !== null) facts.push({ label: 'Сброс', value: fmt.clock(reset) });
    return {
      kind: 'limit_resumed',
      icon: 'hourglass',
      title: provider ? `Лимит ${provider} сброшен — продолжаю` : 'Лимит сброшен — продолжаю',
      tone: 'neutral',
      facts,
      rows: [],
      text
    };
  }
  if (event.kind !== 'quota_paused') return null;
  const title = origin?.title?.trim() || 'помощник';
  const href = event.session_id ? `/c/${event.session_id}` : null;
  if (event.reason === 'no_live_execution') {
    return {
      kind: 'quota_note',
      icon: 'hourglass',
      title: `Лимит сброшен, но «${title}» сам не продолжит`,
      titleHref: href ?? undefined,
      tone: 'neutral',
      reason: 'Отправьте ему сообщение или остановите',
      facts: [],
      rows: [],
      text
    };
  }
  const provider = providerName(event.provider);
  const window = event.window ? windowLabel(event.window) : null;
  const base = `Помощник «${title}» упёрся в лимит${provider ? ` ${provider}` : ''}${window ? ` (${window})` : ''}`;
  const reset = stamp(event.resets_at ?? event.until);
  return {
    kind: 'quota_paused',
    icon: 'hourglass',
    title: reset !== null ? `${base} · сброс ${fmt.clock(reset)}` : base,
    titleHref: href ?? undefined,
    tone: 'neutral',
    facts: [],
    rows: [],
    text
  };
}

function withoutServiceHeader(text: string): string {
  const [head, ...rest] = text.split('\n\n');
  return head?.startsWith('[canterlot]') && rest.length > 0 ? rest.join('\n\n') : text;
}

function firstLines(text: string, count: number): string | null {
  const lines = text.split('\n');
  if (lines.length <= count) return null;
  return `${lines.slice(0, count).join('\n')}\n…`;
}

/** Сообщение или бриф от другой сессии: заголовок, модель, ссылка и тело. */
export function sessionMessageView(
  origin: InputOrigin | null | undefined,
  text: string
): SessionMessageView | null {
  if (origin?.kind !== 'message' && origin?.kind !== 'spawn') return null;
  const deleted = origin.sender_deleted === true;
  const title = origin.title?.trim();
  const name = title || (deleted ? 'удалённая сессия' : 'сессия');
  const heading =
    origin.kind === 'spawn'
      ? `Задание от сессии «${name}»`
      : deleted && !title
        ? 'Сессия удалена'
        : `Сессия «${name}»`;
  const body = withoutServiceHeader(text);
  return {
    kind: origin.kind,
    title: heading,
    model: deleted ? null : origin.model?.trim() ? modelLabel(origin.model.trim()) : null,
    href: deleted || !origin.from ? null : `/c/${origin.from}`,
    body,
    folded: origin.kind === 'spawn' ? firstLines(body, 12) : null
  };
}
