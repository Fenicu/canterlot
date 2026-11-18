import { fmt } from '#lib/format/ru.js';

/** Ожидание дольше порога красит подпись: десять минут — уже повод вмешаться. */
export const WAIT_WARN_MS = 10 * 60_000;

export type WaitKind = 'permission' | 'question' | 'elicitation' | 'spawn_approval' | 'settings_change' | 'unknown';

export interface WaitFacts {
  kind: WaitKind;
  /** Инструмент, о котором просят разрешение: «Bash». */
  tool?: string | null;
  /** Сколько вариантов у первого вопроса. */
  options?: number | null;
  /** Время создания запроса на хабе, мс; `null` — хаб его не прислал. */
  createdAt?: number | null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : {};
}

function text(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function timeMs(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  if (typeof value !== 'string') return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** Инструмент запроса разрешения: Claude кладёт имя наверх, Codex — внутрь request_permission. */
export function permissionToolName(payload: unknown): string {
  const rec = record(payload);
  const direct = text(rec.tool_name) || text(rec.toolName);
  if (direct) return direct;
  const nested = record(rec.request_permission);
  return text(nested.tool_name) || text(nested.toolName);
}

/** Запрос ссылки у MCP-сервера: это разрешение с url-режимом, а не обычный инструмент. */
export function isUrlElicitation(payload: unknown): boolean {
  const rec = record(payload);
  return permissionToolName(payload) === 'mcp_elicitation' && text(rec.mode) === 'url' && Boolean(text(rec.url));
}

/** Варианты первого вопроса: подпись строки говорит, сколько их, а не какой вопрос. */
export function questionOptionCount(payload: unknown): number | null {
  const rec = record(payload);
  const nested = record(rec.payload);
  const list = Array.isArray(rec.questions)
    ? rec.questions
    : Array.isArray(nested.questions)
      ? nested.questions
      : Array.isArray(rec.ask_question && record(rec.ask_question).questions)
        ? (record(rec.ask_question).questions as unknown[])
        : null;
  if (list) {
    const first = record(list[0]);
    return Array.isArray(first.options) ? first.options.length : null;
  }
  return Array.isArray(rec.options) ? rec.options.length : null;
}

/** Факты ожидания из открытого запроса клиента: вид, инструмент, число вариантов, время создания. */
export function waitFactsFromRequest(kind: string, payload: unknown, createdAt: string | number | null | undefined): WaitFacts {
  if (kind === 'elicitation' || isUrlElicitation(payload)) return { kind: 'elicitation', createdAt: timeMs(createdAt) };
  if (kind === 'permission') {
    return { kind: 'permission', tool: permissionToolName(payload) || null, createdAt: timeMs(createdAt) };
  }
  if (kind === 'spawn_approval') return { kind: 'spawn_approval', createdAt: timeMs(createdAt) };
  if (kind === 'settings_change') return { kind: 'settings_change', createdAt: timeMs(createdAt) };
  return { kind: 'question', options: questionOptionCount(payload), createdAt: timeMs(createdAt) };
}

/** Имя инструмента в начале сводки разрешения: «Bash: cargo test» или «Bash». */
function toolFromSummary(summary: string): string | null {
  const match = /^([A-Za-z][\w.-]*)(?::|\s*$)/.exec(summary);
  return match ? match[1]! : null;
}

/** Похожа ли сводка на разрешение инструмента, а не на текст вопроса. */
function looksLikeTool(summary: string): boolean {
  return toolFromSummary(summary) !== null;
}

/**
 * Факты ожидания из активности списка. Хаб кладёт в неё фазу исполнения и сводку запроса;
 * вид запроса и число вариантов приходят, только если хаб их прислал.
 */
export function waitFactsFromActivity(activity: {
  state: string;
  evidence?: unknown;
  since?: string;
}): WaitFacts | null {
  if (activity.state !== 'waiting_for_user') return null;
  const ev = record(activity.evidence);
  const explicit = text(ev.request_kind);
  const phase = text(ev.phase);
  const summary = text(ev.question);
  const createdAt = timeMs(ev.request_created_at) ?? timeMs(activity.since);
  if (explicit === 'permission' || explicit === 'question' || explicit === 'elicitation' || explicit === 'spawn_approval' || explicit === 'settings_change') {
    return {
      kind: explicit,
      tool: text(ev.request_tool) || (explicit === 'permission' ? toolFromSummary(summary) : null),
      options: typeof ev.option_count === 'number' ? ev.option_count : null,
      createdAt
    };
  }
  // Фаза различает вопрос и разрешение у Claude Code; у Codex вопрос приходит с фазой
  // разрешения, поэтому сводку без имени инструмента считаем вопросом.
  if (phase === 'awaiting_answer') {
    return { kind: 'question', options: typeof ev.option_count === 'number' ? ev.option_count : null, createdAt };
  }
  if (phase === 'awaiting_permission') {
    const tool = looksLikeTool(summary) ? toolFromSummary(summary) : null;
    return { kind: tool || !summary ? 'permission' : 'question', tool, createdAt };
  }
  return null;
}

/** Возраст запроса словами длительности: «12 мин», «1 ч 5 мин». */
export function waitAge(facts: WaitFacts, now: number): string | null {
  if (facts.createdAt === null || facts.createdAt === undefined) return null;
  return fmt.durationCoarse(Math.max(0, now - facts.createdAt));
}

/** Строка ожидания для списка и шапки: вид, подробность и возраст. */
export function waitStatusText(facts: WaitFacts, now: number): string {
  const age = waitAge(facts, now);
  const parts: string[] = [];
  switch (facts.kind) {
    case 'permission':
      parts.push('Ждёт разрешения');
      if (facts.tool) parts.push(facts.tool);
      break;
    case 'question':
      parts.push('Вопрос');
      if (facts.options && facts.options > 0) {
        parts.push(`${facts.options} ${fmt.plural(facts.options, ['вариант', 'варианта', 'вариантов'])}`);
      }
      break;
    case 'elicitation':
      parts.push('Ждёт ссылку');
      break;
    case 'spawn_approval':
      parts.push('Ждёт одобрения запуска');
      break;
    case 'settings_change':
      parts.push('Ждёт решения по настройке');
      break;
    default:
      parts.push('Ждёт ответа');
  }
  if (age) parts.push(age);
  return parts.join(' · ');
}

/** Шапка карточки: «Запрос разрешения · 12 мин назад». */
export function waitHeadText(facts: WaitFacts, now: number): string {
  const label =
    facts.kind === 'question'
      ? 'Вопрос'
      : facts.kind === 'elicitation'
        ? 'Запрос MCP-сервера'
        : facts.kind === 'spawn_approval'
          ? 'Запрос на запуск'
          : facts.kind === 'settings_change'
            ? 'Предложение настройки'
            : facts.kind === 'permission'
            ? 'Запрос разрешения'
            : 'Запрос';
  const age = waitAge(facts, now);
  return age ? `${label} · ${age} назад` : label;
}

/** Долгое ожидание: карточка переходит в предупреждающий тон. */
export function waitIsLong(facts: WaitFacts, now: number): boolean {
  if (facts.createdAt === null || facts.createdAt === undefined) return false;
  return now - facts.createdAt > WAIT_WARN_MS;
}
