// Общая память агентов: поиск, чтение, запись, правка и архив. Ответы хаба: `session_tools/memory.rs`.
import { fmt } from '#lib/format/ru.js';
import { kindWord } from '#lib/screens/settings/memory.js';
import { isObject, list, num, str, type JsonObject } from '../json';
import { say } from '../copy';
import type { CardCall, CardLogic, StateWord } from '../types';

// Словарь видов один, на экране «Память»: карточки берут слова оттуда.
export { kindWord };

/** Уровень записи: проект называется по подписи, остальное — «весь домен». */
export function levelWord(level: string, projectLabel: string): string {
  return level === 'project' ? projectLabel || 'проект' : 'весь домен';
}

/** Адрес записи на экране «Память»; домен подсказывает, где её искать, если он известен. */
export function memoryHref(id: string, domain = ''): string {
  const hint = domain === 'work' || domain === 'personal' ? `&domain=${domain}` : '';
  return `/settings?c=memory&entry=${encodeURIComponent(id)}${hint}`;
}

export function shortId(id: string): string {
  return id.slice(0, 8);
}

/** Подпись записи одной строкой: вид, уровень, версия и срок, что из этого известно. */
export function entryMeta(entry: { kind: string; level: string; projectLabel: string; version?: number | null; validUntil?: number | null; expired?: boolean }): string {
  const parts = [entry.kind ? kindWord(entry.kind) : '', entry.level ? levelWord(entry.level, entry.projectLabel) : ''];
  if (entry.version) parts.push(`версия ${entry.version}`);
  if (entry.validUntil) parts.push(`${entry.expired ? 'истёк' : 'до'} ${fmt.shortDate(entry.validUntil)}`);
  return parts.filter(Boolean).join(' · ');
}

function time(value: unknown): number | null {
  const ts = typeof value === 'string' ? Date.parse(value) : Number.NaN;
  return Number.isFinite(ts) ? ts : null;
}

/** Отказ хаба: код — первое слово ответа, подробности — объект JSON за ним. */
export interface MemoryRefusal {
  code: string;
  /** Запись, о которой говорит отказ: уже существующая с той же темой. */
  id: string;
  version: number | null;
  /** Версия записи у хаба при конфликте. */
  current: number | null;
  secretClass: string;
  field: string;
}

export function refusalOf(call: CardCall): MemoryRefusal | null {
  if (call.state !== 'error') return null;
  const first = call.output.split('\n').find((part) => part.trim()) ?? '';
  const meta = call.answer ?? {};
  return {
    code: /^([a-z_]+):/.exec(first.trim())?.[1] ?? '',
    id: str(meta.id),
    version: num(meta.version),
    current: num(meta.current_version),
    secretClass: str(meta.class),
    field: str(meta.field)
  };
}

/** Отказ словами: что не удалось и почему. */
export function refusalLine(refusal: MemoryRefusal): string {
  switch (refusal.code) {
    case 'looks_like_secret':
      return 'Отказ: похоже на секрет';
    case 'subject_exists':
      return 'Уже есть запись с такой темой';
    case 'rate_limited':
      return 'Лимит записей в час';
    case 'too_large':
      return 'Отказ: запись слишком большая';
    case 'version_conflict':
      return refusal.current !== null ? `Отказ: запись уже изменили (версия ${refusal.current})` : 'Отказ: запись уже изменили';
    case 'not_found':
      return 'Отказ: записи нет';
    case 'domain_full':
      return 'Отказ: память домена заполнена';
    case 'bad_level':
      return 'Отказ: у сессии нет проекта, запись только на весь домен';
    case 'invalid':
      return refusal.field ? `Отказ: неверное поле ${refusal.field}` : 'Отказ: неверное поле';
    case 'memory_unavailable':
      return 'Отказ: у этой сессии нет памяти';
    case '':
      return 'Отказ: вызов не удался';
    default:
      return `Отказ: ${refusal.code}`;
  }
}

const running = (call: CardCall): boolean => call.state === 'running';

/* ------------------------------------------------------------------ поиск */

export interface MemoryRow {
  id: string;
  kind: string;
  level: string;
  projectLabel: string;
  subject: string;
  excerpt: string;
  updatedAt: number | null;
  expired: boolean;
}

export interface MemorySearchData {
  query: string;
  results: MemoryRow[];
  refusal: MemoryRefusal | null;
}

function rowOf(value: unknown): MemoryRow | null {
  if (!isObject(value as never)) return null;
  const item = value as JsonObject;
  const id = str(item.id);
  if (!id) return null;
  return {
    id,
    kind: str(item.kind),
    level: str(item.level),
    projectLabel: str(item.project_label),
    subject: str(item.subject),
    excerpt: str(item.excerpt),
    updatedAt: time(item.updated_at),
    expired: item.expired === true
  };
}

export const memorySearch: CardLogic<MemorySearchData> = {
  label: 'Память',
  parse(call) {
    const refusal = refusalOf(call);
    if (!call.answer && !refusal && !running(call)) return null;
    const results = refusal ? [] : list(call.answer?.results).map(rowOf).filter((row): row is MemoryRow => row !== null);
    return { query: str(call.input.query).trim(), results, refusal };
  },
  summary(call, data) {
    if (data.refusal) return refusalLine(data.refusal);
    if (!data.query) return 'Новые записи памяти';
    return `${call.state === 'done' ? say(call.voice, 'Искала в памяти', 'Поиск в памяти') : 'Поиск в памяти'}: «${data.query}»`;
  }
};

/** Итог поиска справа: сколько записей нашлось. */
export function searchWord(data: MemorySearchData): StateWord {
  const count = data.results.length;
  return { word: count === 0 ? 'ничего' : `${count} ${fmt.plural(count, ['запись', 'записи', 'записей'])}`, tone: 'muted' };
}

/* ------------------------------------------------------------------ чтение */

export interface MemoryGetData {
  id: string;
  domain: string;
  subject: string;
  kind: string;
  level: string;
  projectLabel: string;
  body: string;
  version: number | null;
  validUntil: number | null;
  expired: boolean;
  refusal: MemoryRefusal | null;
}

export const memoryGet: CardLogic<MemoryGetData> = {
  label: 'Память',
  parse(call) {
    const refusal = refusalOf(call);
    const answer = call.answer;
    const subject = str(answer?.subject);
    if (!refusal && !subject && !(running(call) && str(call.input.id))) return null;
    return {
      id: str(answer?.id) || str(call.input.id),
      domain: str(answer?.domain),
      subject: refusal ? '' : subject,
      kind: str(answer?.kind),
      level: str(answer?.level),
      projectLabel: str(answer?.project_label),
      body: str(answer?.body),
      version: num(answer?.version),
      validUntil: time(answer?.valid_until),
      expired: answer?.expired === true,
      refusal
    };
  },
  summary(call, data) {
    if (data.refusal) return refusalLine(data.refusal);
    if (!data.subject) return 'Чтение записи';
    return `${say(call.voice, 'Прочитала запись', 'Чтение записи')}: ${data.subject}`;
  },
  link: (_call, data) => (data.subject && data.id ? { text: data.subject, href: memoryHref(data.id, data.domain) } : null)
};

/* ------------------------------------------------------------------ запись */

export interface MemoryWriteData {
  id: string;
  domain: string;
  subject: string;
  kind: string;
  level: string;
  projectLabel: string;
  body: string;
  /** Текст вызова оборван: тело пришло не целиком. */
  bodyCut: boolean;
  validUntil: number | null;
  version: number | null;
  refusal: MemoryRefusal | null;
}

export const memoryWrite: CardLogic<MemoryWriteData> = {
  stays: true,
  label: 'Память',
  parse(call) {
    const subject = str(call.input.subject).trim();
    if (!subject) return null;
    return {
      id: str(call.answer?.id),
      domain: str(call.answer?.domain),
      subject,
      kind: str(call.input.kind),
      // Без уровня в вызове хаб пишет на проект сессии.
      level: str(call.answer?.level) || str(call.input.level) || 'project',
      projectLabel: str(call.answer?.project_label),
      body: str(call.input.body),
      bodyCut: call.partial,
      validUntil: time(call.input.valid_until),
      version: num(call.answer?.version),
      refusal: refusalOf(call)
    };
  },
  summary(call, data) {
    if (data.refusal) return refusalLine(data.refusal);
    return `${call.state === 'done' ? say(call.voice, 'Запомнила', 'Запись в память') : 'Запись в память'}: ${data.subject}`;
  },
  link(_call, data) {
    if (data.refusal) return data.refusal.code === 'subject_exists' && data.refusal.id ? { text: 'запись', href: memoryHref(data.refusal.id) } : null;
    return data.id ? { text: data.subject, href: memoryHref(data.id, data.domain) } : null;
  }
};

/* ------------------------------------------------------------------ правка */

export type MemoryChange = 'subject' | 'kind' | 'level' | 'expiry' | 'text';

export const CHANGE_WORD: Record<MemoryChange, string> = {
  subject: 'тема',
  kind: 'вид',
  level: 'уровень',
  expiry: 'срок',
  text: 'текст'
};

export interface MemoryUpdateData {
  id: string;
  domain: string;
  /** Как называть запись в строке: тема из прошлого чтения или поиска, новая тема, начало id. */
  name: string;
  expectedVersion: number | null;
  newVersion: number | null;
  changes: MemoryChange[];
  subject: string;
  kind: string;
  level: string;
  body: string;
  bodyCut: boolean;
  validUntil: number | null;
  expiryCleared: boolean;
  refusal: MemoryRefusal | null;
}

export const memoryUpdate: CardLogic<MemoryUpdateData> = {
  stays: true,
  label: 'Память',
  parse(call) {
    const id = str(call.input.id);
    const refusal = refusalOf(call);
    if (!id || (!call.answer && !refusal && !running(call))) return null;
    const has = (key: string) => Object.hasOwn(call.input, key);
    const changes: MemoryChange[] = [];
    if (has('subject')) changes.push('subject');
    if (has('kind')) changes.push('kind');
    if (has('level')) changes.push('level');
    if (has('valid_until')) changes.push('expiry');
    if (has('body')) changes.push('text');
    const subject = str(call.input.subject).trim();
    return {
      id,
      domain: str(call.answer?.domain),
      name: call.memorySubject(id) || subject || shortId(id),
      expectedVersion: num(call.input.expected_version),
      newVersion: num(call.answer?.version),
      changes,
      subject,
      kind: str(call.input.kind),
      level: str(call.input.level),
      body: str(call.input.body),
      bodyCut: call.partial,
      validUntil: time(call.input.valid_until),
      expiryCleared: has('valid_until') && call.input.valid_until === null,
      refusal
    };
  },
  summary(call, data) {
    if (data.refusal) return refusalLine(data.refusal);
    return `${call.state === 'done' ? say(call.voice, 'Обновила запись', 'Обновление записи') : 'Обновление записи'}: ${data.name}`;
  },
  link: (_call, data) => (!data.refusal && data.name && data.id ? { text: data.name, href: memoryHref(data.id, data.domain) } : null)
};

/** Справа: «версия 4 → 5». */
export function updateWord(data: MemoryUpdateData): StateWord | null {
  return data.expectedVersion !== null && data.newVersion !== null ? { word: `версия ${data.expectedVersion} → ${data.newVersion}`, tone: 'muted' } : null;
}

/* ------------------------------------------------------------------ архив */

export interface MemoryDeleteData {
  id: string;
  name: string;
  refusal: MemoryRefusal | null;
}

export const memoryDelete: CardLogic<MemoryDeleteData> = {
  stays: true,
  label: 'Память',
  parse(call) {
    const id = str(call.input.id);
    const refusal = refusalOf(call);
    if (!id || (!call.answer && !refusal && !running(call))) return null;
    return { id, name: call.memorySubject(id) || shortId(id), refusal };
  },
  summary(call, data) {
    if (data.refusal) return refusalLine(data.refusal);
    return `${call.state === 'done' ? say(call.voice, 'Убрала в архив', 'Архивация записи') : 'Архивация записи'}: ${data.name}`;
  }
};
