// Ответ инструмента `session_spawn` в ленте оркестратора: запуск по маршруту или очередь.
import type { RouteDecision } from './decision';

export interface SpawnAnswer {
  status: string;
  session_id?: string | null;
  routing?: Partial<RouteDecision> | null;
  queue_id?: string | null;
  reason?: string | null;
  detail?: string | null;
  not_before?: string | null;
}

/** Вызов `session_spawn` любого харнесса: `mcp__canterlot__session_spawn`, `canterlot_session_spawn`. */
export function isSpawnCall(toolName: string | null | undefined): boolean {
  return /(^|[_.])session_spawn$/.test(toolName ?? '');
}

/** JSON ответа хаба внутри текста результата; `null`, если это не ответ запуска. */
export function parseSpawnAnswer(text: string | null | undefined): SpawnAnswer | null {
  const raw = text ?? '';
  const start = raw.indexOf('{');
  const end = raw.lastIndexOf('}');
  if (start < 0 || end <= start) return null;
  try {
    const value = JSON.parse(raw.slice(start, end + 1)) as unknown;
    if (!value || typeof value !== 'object') return null;
    const answer = value as SpawnAnswer;
    return typeof answer.status === 'string' ? answer : null;
  } catch {
    return null;
  }
}
