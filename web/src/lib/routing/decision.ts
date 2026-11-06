// Решение маршрута для строк ленты, шапки и предпросмотра: пара, усилие, пропуски.
import { effortLabel, modelLabel, providerLabel } from '#lib/format/labels.js';
import { reasonText, verdictLabel } from './reasons';

export interface Considered {
  harness: string;
  model: string;
  verdict: string;
  reason: string;
}

export interface RouteDecision {
  policy_revision?: number;
  task_type: string;
  level?: string | null;
  harness: string;
  model: string;
  effort: string;
  preset?: string | null;
  nudge?: unknown;
  watchdog_minutes?: number | null;
  considered: Considered[];
}

/** Каталог машин: название модели вместо идентификатора, где он известен. */
export type ModelTitles = (harness: string, model: string) => string | undefined;

const HARNESS_PROVIDER: Record<string, string> = {
  claude_code: 'claude',
  codex: 'codex',
  opencode: 'opencode',
  antigravity: 'antigravity'
};

export function harnessName(harness: string): string {
  const label = providerLabel(HARNESS_PROVIDER[harness] ?? harness);
  return label === 'Другой провайдер' ? harness : label;
}

/** Короткое имя пары: модель по каталогу, иначе по идентификатору; `default` — харнесс. */
export function pairName(harness: string, model: string, titles?: ModelTitles): string {
  if (!model || model === 'default') return harnessName(harness);
  return titles?.(harness, model) ?? modelLabel(model);
}

export function asDecision(value: unknown): RouteDecision | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  if (typeof record.task_type !== 'string' || typeof record.harness !== 'string' || typeof record.model !== 'string') return null;
  return {
    ...(record as unknown as RouteDecision),
    effort: typeof record.effort === 'string' ? record.effort : '',
    considered: Array.isArray(record.considered) ? (record.considered as Considered[]) : []
  };
}

/** Причина пропуска без повтора имени провайдера: «Codex — неделя 83 %…» уже называет его. */
export function skippedText(entry: Considered, titles?: ModelTitles, now?: number): string {
  const name = pairName(entry.harness, entry.model, titles);
  const reason = reasonText(entry.reason, now) || verdictLabel(entry.verdict);
  const provider = harnessName(entry.harness);
  if (reason.startsWith(`${provider} — `)) {
    const rest = reason.slice(provider.length + 3);
    return name === provider ? `${provider} — ${rest}` : `${name} — ${rest}`;
  }
  return `${name} — ${reason}`;
}

export function skipped(decision: RouteDecision): Considered[] {
  return decision.considered.filter((entry) => entry.verdict !== 'chosen');
}

export function chosenText(decision: RouteDecision, titles?: ModelTitles): string {
  const name = pairName(decision.harness, decision.model, titles);
  return decision.effort ? `${name} · ${effortName(decision.effort)}` : name;
}

/** Усилие строчными внутри строки; незнакомое — как прислал хаб. */
export function effortName(effort: string): string {
  const label = effortLabel(effort);
  return label === 'Другое усилие' ? effort : label.toLocaleLowerCase('ru-RU');
}

/** «Маршрут «Исправление»: DeepSeek · высокое. Пропущены: Codex — неделя 83 % при пороге 80 %». */
export function routeLine(decision: RouteDecision, title: string, titles?: ModelTitles, now?: number): string {
  const head = `Маршрут «${title || decision.task_type}»: ${chosenText(decision, titles)}`;
  const rest = skipped(decision).map((entry) => skippedText(entry, titles, now));
  return rest.length > 0 ? `${head}. ${rest.length === 1 ? 'Пропущен' : 'Пропущены'}: ${rest.join('; ')}` : head;
}

/** Названия моделей из каталогов машин: первая машина, знающая пару, называет её. */
export function catalogTitles(
  machines: Iterable<{ harnesses?: { id: string; models: { id: string; title: string }[] }[] }>
): ModelTitles {
  const titles = new Map<string, string>();
  for (const machine of machines)
    for (const harness of machine.harnesses ?? [])
      for (const model of harness.models) {
        const key = `${harness.id}/${model.id}`;
        if (!titles.has(key) && model.title) titles.set(key, model.title);
      }
  return (harness, model) => titles.get(`${harness}/${model}`);
}
