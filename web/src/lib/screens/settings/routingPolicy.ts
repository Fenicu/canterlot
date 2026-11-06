// Модель редактора политики маршрутизации: черновик, проверка по правилам хаба,
// перестановка кандидатов и признак изменений. Компонент только рисует её.
import type { components } from '#lib/api/generated/openapi.js';
import type { WhitelistEntry } from './spawnWhitelist.js';

export type Policy = components['schemas']['Policy'];
export type PolicyCandidate = components['schemas']['Candidate'];
export type PolicyTask = components['schemas']['TaskType'];

export const MAX_TASK_TYPES = 32;
export const MAX_CANDIDATES = 8;
export const MAX_EXCLUDE_WORKDIRS = 16;
export const LEVELS = ['low', 'medium', 'high', 'xhigh'] as const;
export const PROVIDERS = ['claude', 'codex', 'opencode'] as const;
export const KNOWN_HARNESSES = ['claude_code', 'codex', 'opencode', 'antigravity'];
export const NOT_IN_WHITELIST = 'Не в белом списке — маршрут их пропустит';

export type NudgeMode = 'off' | 'on' | 'max';

export interface DraftCandidate {
  uid: string;
  harness: string;
  model: string;
  /** Пусто: усилие по уровню или по умолчанию модели. */
  effort: string;
  /** Пусто: кандидат годится для любого уровня. */
  levels: string[];
  nudge: NudgeMode;
  maxNudges: number;
  /** Прочие поля правила автопинка, которых редактор не показывает: сохраняются как были. */
  nudgeRest: Record<string, unknown>;
  watchdog: number | null;
  exclude: string[];
  preset: string | null;
}

export interface DraftTask {
  uid: string;
  key: string;
  title: string;
  preset: string | null;
  isolated: boolean | null;
  candidates: DraftCandidate[];
}

export interface DraftProvider {
  provider: string;
  weekly: number | null;
  fiveHour: number | null;
  monthly: number | null;
}

export interface DraftParallel {
  uid: string;
  pair: string;
  limit: number;
}

export interface Draft {
  tasks: DraftTask[];
  providers: DraftProvider[];
  parallel: DraftParallel[];
  unlimited: string[];
  unknownLimits: 'allow' | 'skip';
}

export interface Issue {
  /** Где ошибка: `task:<uid>`, `candidate:<uid>`, `provider:<name>`, `parallel:<uid>`. */
  at: string;
  message: string;
}

let serial = 0;
export function uid(prefix = 'u'): string {
  serial += 1;
  return `${prefix}${serial}`;
}

function nudgeOf(value: unknown): Pick<DraftCandidate, 'nudge' | 'maxNudges' | 'nudgeRest'> {
  if (value === true) return { nudge: 'on', maxNudges: 3, nudgeRest: {} };
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    const { max_nudges, ...rest } = value as Record<string, unknown>;
    if (rest.enabled === false) return { nudge: 'off', maxNudges: 3, nudgeRest: {} };
    return typeof max_nudges === 'number'
      ? { nudge: 'max', maxNudges: max_nudges, nudgeRest: rest }
      : { nudge: 'on', maxNudges: 3, nudgeRest: rest };
  }
  return { nudge: 'off', maxNudges: 3, nudgeRest: {} };
}

export function candidateDraft(input: Partial<PolicyCandidate> & { harness: string; model: string }): DraftCandidate {
  return {
    uid: uid('c'),
    harness: input.harness,
    model: input.model,
    effort: input.effort ?? '',
    levels: input.levels ? [...input.levels] : [],
    ...nudgeOf(input.nudge),
    watchdog: input.watchdog_minutes ?? null,
    exclude: [...(input.exclude_workdirs ?? [])],
    preset: input.preset ?? null
  };
}

export function fromPolicy(policy: Policy): Draft {
  const providers = new Map<string, DraftProvider>();
  for (const name of PROVIDERS) providers.set(name, { provider: name, weekly: null, fiveHour: null, monthly: null });
  for (const [name, limits] of Object.entries(policy.providers ?? {})) {
    providers.set(name, {
      provider: name,
      weekly: limits.max_weekly_percent ?? null,
      fiveHour: limits.max_five_hour_percent ?? null,
      monthly: limits.max_monthly_percent ?? null
    });
  }
  return {
    tasks: Object.entries(policy.task_types ?? {}).map(([key, task]) => ({
      uid: uid('t'),
      key,
      title: task.title,
      preset: task.preset ?? null,
      isolated: task.isolated ?? null,
      candidates: task.candidates.map((candidate) => candidateDraft(candidate))
    })),
    providers: [...providers.values()],
    parallel: Object.entries(policy.max_parallel ?? {}).map(([pair, limit]) => ({ uid: uid('p'), pair, limit })),
    unlimited: [...(policy.unlimited_models ?? [])],
    unknownLimits: policy.unknown_limits === 'skip' ? 'skip' : 'allow'
  };
}

function nudgeValue(candidate: DraftCandidate): unknown {
  if (candidate.nudge === 'off') return false;
  if (candidate.nudge === 'max') return { ...candidate.nudgeRest, max_nudges: candidate.maxNudges };
  return Object.keys(candidate.nudgeRest).length > 0 ? { ...candidate.nudgeRest } : true;
}

function candidateValue(candidate: DraftCandidate): PolicyCandidate {
  const out: PolicyCandidate = { harness: candidate.harness, model: candidate.model.trim() };
  if (candidate.effort) out.effort = candidate.effort;
  if (candidate.levels.length > 0) out.levels = LEVELS.filter((level) => candidate.levels.includes(level));
  out.nudge = nudgeValue(candidate);
  if (candidate.watchdog !== null) out.watchdog_minutes = candidate.watchdog;
  out.exclude_workdirs = [...candidate.exclude];
  if (candidate.preset) out.preset = candidate.preset;
  return out;
}

export function toPolicy(draft: Draft): Policy {
  const task_types: Record<string, PolicyTask> = {};
  for (const task of draft.tasks) {
    const value: PolicyTask = { title: task.title.trim(), candidates: task.candidates.map(candidateValue) };
    if (task.preset) value.preset = task.preset;
    if (task.isolated !== null) value.isolated = task.isolated;
    task_types[task.key.trim()] = value;
  }
  const providers: Policy['providers'] = {};
  for (const item of draft.providers) {
    if (item.weekly === null && item.fiveHour === null && item.monthly === null) continue;
    providers[item.provider] = {
      ...(item.weekly !== null ? { max_weekly_percent: item.weekly } : {}),
      ...(item.fiveHour !== null ? { max_five_hour_percent: item.fiveHour } : {}),
      ...(item.monthly !== null ? { max_monthly_percent: item.monthly } : {})
    };
  }
  const max_parallel: Record<string, number> = {};
  for (const item of draft.parallel) if (item.pair.trim()) max_parallel[item.pair.trim()] = item.limit;
  return {
    task_types,
    providers,
    max_parallel,
    unlimited_models: draft.unlimited.map((item) => item.trim()).filter(Boolean),
    unknown_limits: draft.unknownLimits
  };
}

/** Сравнение по сохраняемой форме: порядок ключей объектов не важен, порядок кандидатов важен. */
function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, item]) => [key, canonical(item)])
    );
  return value;
}

export function samePolicy(a: Policy, b: Policy): boolean {
  return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
}

export function isDirty(draft: Draft, saved: Policy): boolean {
  return !samePolicy(toPolicy(draft), toPolicy(fromPolicy(saved)));
}

const KEY = /^[a-z0-9_]{1,32}$/;

function percentIssue(value: number | null): boolean {
  return value !== null && (!Number.isFinite(value) || value < 0 || value > 100);
}

export function validate(draft: Draft): Issue[] {
  const issues: Issue[] = [];
  if (draft.tasks.length === 0) issues.push({ at: 'tasks', message: 'Нужен хотя бы один тип задачи' });
  if (draft.tasks.length > MAX_TASK_TYPES) issues.push({ at: 'tasks', message: `Не больше ${MAX_TASK_TYPES} типов задач` });
  const keys = new Map<string, number>();
  for (const task of draft.tasks) keys.set(task.key.trim(), (keys.get(task.key.trim()) ?? 0) + 1);
  for (const task of draft.tasks) {
    const at = `task:${task.uid}`;
    const key = task.key.trim();
    if (!KEY.test(key)) issues.push({ at, message: 'Ключ: строчные латинские буквы, цифры и «_», до 32 знаков' });
    else if ((keys.get(key) ?? 0) > 1) issues.push({ at, message: `Ключ «${key}» уже занят` });
    if (!task.title.trim()) issues.push({ at, message: 'Нужно название' });
    if (task.candidates.length === 0) issues.push({ at, message: 'Нужен хотя бы один кандидат' });
    if (task.candidates.length > MAX_CANDIDATES) issues.push({ at, message: `Не больше ${MAX_CANDIDATES} кандидатов` });
    for (const candidate of task.candidates) {
      const cat = `candidate:${candidate.uid}`;
      if (!KNOWN_HARNESSES.includes(candidate.harness)) issues.push({ at: cat, message: `Неизвестный агент ${candidate.harness}` });
      if (!candidate.model.trim()) issues.push({ at: cat, message: 'Нужна модель' });
      if (candidate.watchdog !== null && (!Number.isInteger(candidate.watchdog) || candidate.watchdog < 5 || candidate.watchdog > 1440))
        issues.push({ at: cat, message: 'Сторож: от 5 до 1440 минут' });
      if (candidate.nudge === 'max' && (!Number.isInteger(candidate.maxNudges) || candidate.maxNudges < 1 || candidate.maxNudges > 10))
        issues.push({ at: cat, message: 'Толчков: от 1 до 10' });
      if (candidate.exclude.length > MAX_EXCLUDE_WORKDIRS) issues.push({ at: cat, message: `Не больше ${MAX_EXCLUDE_WORKDIRS} исключённых каталогов` });
      for (const path of candidate.exclude)
        if (!path.startsWith('/')) issues.push({ at: cat, message: `Каталог «${path}» должен начинаться с /` });
    }
  }
  for (const item of draft.providers) {
    if ([item.weekly, item.fiveHour, item.monthly].some(percentIssue))
      issues.push({ at: `provider:${item.provider}`, message: 'Порог: от 0 до 100 %' });
  }
  const pairs = new Set<string>();
  for (const item of draft.parallel) {
    const at = `parallel:${item.uid}`;
    const pair = item.pair.trim();
    if (!/^[a-z_-]+\/.+$/.test(pair)) issues.push({ at, message: 'Пара в виде агент/модель' });
    else if (pairs.has(pair)) issues.push({ at, message: `Пара «${pair}» уже есть` });
    pairs.add(pair);
    if (!Number.isInteger(item.limit) || item.limit < 1) issues.push({ at, message: 'Не меньше одной сессии' });
  }
  return issues;
}

export function issuesAt(issues: Issue[], at: string): string[] {
  return issues.filter((issue) => issue.at === at).map((issue) => issue.message);
}

/** Перестановка: элемент `from` встаёт на место `to`; вне границ — без изменений. */
export function move<T>(list: T[], from: number, to: number): T[] {
  if (from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return list;
  const next = [...list];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item!);
  return next;
}

export function moveCandidate(draft: Draft, taskUid: string, from: number, to: number): Draft {
  return {
    ...draft,
    tasks: draft.tasks.map((task) => (task.uid === taskUid ? { ...task, candidates: move(task.candidates, from, to) } : task))
  };
}

export function addTask(draft: Draft, first: { harness: string; model: string }): Draft | string {
  if (draft.tasks.length >= MAX_TASK_TYPES) return `Не больше ${MAX_TASK_TYPES} типов задач`;
  let n = draft.tasks.length + 1;
  while (draft.tasks.some((task) => task.key === `task_${n}`)) n += 1;
  return {
    ...draft,
    tasks: [
      ...draft.tasks,
      { uid: uid('t'), key: `task_${n}`, title: 'Новый тип', preset: null, isolated: null, candidates: [candidateDraft(first)] }
    ]
  };
}

export function removeTask(draft: Draft, taskUid: string): Draft {
  return { ...draft, tasks: draft.tasks.filter((task) => task.uid !== taskUid) };
}

export function addCandidate(draft: Draft, taskUid: string, pair: { harness: string; model: string }): Draft | string {
  const task = draft.tasks.find((item) => item.uid === taskUid);
  if (!task) return draft;
  if (task.candidates.length >= MAX_CANDIDATES) return `Не больше ${MAX_CANDIDATES} кандидатов`;
  return {
    ...draft,
    tasks: draft.tasks.map((item) => (item.uid === taskUid ? { ...item, candidates: [...item.candidates, candidateDraft(pair)] } : item))
  };
}

export function removeCandidate(draft: Draft, taskUid: string, candidateUid: string): Draft {
  return {
    ...draft,
    tasks: draft.tasks.map((task) =>
      task.uid === taskUid ? { ...task, candidates: task.candidates.filter((candidate) => candidate.uid !== candidateUid) } : task
    )
  };
}

/** Как `whitelist::allows` хаба: запись `default` разрешает первую модель каталога. */
export function allowedByWhitelist(
  entries: WhitelistEntry[],
  harness: string,
  model: string,
  firstModel?: string
): boolean {
  return entries.some(
    (entry) =>
      entry.harness === harness &&
      (entry.model === model || (entry.model === 'default' && (model === 'default' || model === firstModel)))
  );
}
