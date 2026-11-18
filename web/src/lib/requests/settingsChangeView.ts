// Человекочитаемая разница для карточки предложения изменить настройку.
// Хаб присылает полные значения «до» и «после»; здесь из них собирается только то,
// что реально изменилось, и считается число неизменённых частей.
import { effortLabel } from '#lib/format/labels.js';

export type DiffMark = 'added' | 'removed' | 'changed' | 'context';

export interface DiffLine {
  mark: DiffMark;
  text: string;
}

export interface DiffResult {
  lines: DiffLine[];
  unchanged: number;
}

export type SettingKind = 'routing' | 'whitelist' | 'preset' | 'unknown';

export interface SettingsChangeView extends DiffResult {
  setting: SettingKind;
  title: string;
  reason: string;
  baseRevision: number;
  /** Название пресета показываем отдельно от разницы текста. */
  presetName?: string;
}

export type OutcomeTone = 'done' | 'danger' | 'warning' | 'quiet';

export interface SettingsOutcome {
  word: string;
  tone: OutcomeTone;
}

export interface Policy {
  task_types?: Record<string, TaskType>;
  providers?: Record<string, Record<string, unknown>>;
  max_parallel?: Record<string, number>;
  unlimited_models?: string[];
  unknown_limits?: string;
}

interface TaskType {
  title?: string;
  candidates?: Record<string, unknown>[];
  preset?: string | null;
  isolated?: boolean | null;
  expected_minutes?: number | null;
}

export type WhitelistEntry = { harness?: string; model?: string };

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function shown(value: unknown): string {
  if (value === null || value === undefined || value === '') return 'нет';
  if (Array.isArray(value)) return value.length ? value.map(shown).join(', ') : 'нет';
  return String(value);
}

function lowerEffort(value: unknown): string {
  const label = effortLabel(text(value));
  return label === 'Другое усилие' ? text(value) : label.toLocaleLowerCase('ru-RU');
}

function nudgeLabel(value: unknown): string {
  if (value === true) return 'включён';
  if (value && typeof value === 'object') {
    const item = record(value);
    if (item.enabled === false) return 'выключен';
    if (typeof item.max_nudges === 'number') return `до ${item.max_nudges}`;
    return 'включён';
  }
  return 'выключен';
}

function minutes(value: unknown): string {
  return typeof value === 'number' && Number.isFinite(value) ? `${value} мин` : 'нет';
}

function pairKey(value: unknown): string {
  const item = record(value);
  const harness = text(item.harness) || '?';
  const model = text(item.model) || '?';
  return `${harness}/${model}`;
}

function changed(label: string, before: unknown, after: unknown): DiffLine {
  return { mark: 'changed', text: `${label}: ${shown(before)} → ${shown(after)}` };
}

function candidateDiff(before: unknown, after: unknown): { lines: DiffLine[]; unchanged: number } {
  const a = record(before);
  const b = record(after);
  const lines: DiffLine[] = [];
  let unchanged = 0;
  const compare = (label: string, key: string, render: (value: unknown) => string = shown) => {
    if (same(a[key], b[key])) {
      if (a[key] !== undefined || b[key] !== undefined) unchanged += 1;
    } else {
      lines.push(changed(label, render(a[key]), render(b[key])));
    }
  };
  compare('Усилие', 'effort', lowerEffort);
  compare('Автопинг', 'nudge', nudgeLabel);
  compare('Сторож', 'watchdog_minutes', minutes);
  compare('Уровни', 'levels');
  compare('Исключённые каталоги', 'exclude_workdirs');
  compare('Пресет', 'preset');
  return { lines, unchanged };
}

function taskDiff(before: TaskType, after: TaskType): DiffResult {
  const lines: DiffLine[] = [];
  let unchanged = 0;
  if (before.title !== after.title) lines.push(changed('Название', before.title, after.title));
  else unchanged += 1;
  if (!same(before.preset, after.preset)) lines.push(changed('Пресет задачи', before.preset, after.preset));
  if (!same(before.isolated, after.isolated)) lines.push(changed('Отдельная рабочая копия', before.isolated, after.isolated));
  if (!same(before.expected_minutes, after.expected_minutes)) {
    lines.push(changed('Ожидаемая длительность', minutes(before.expected_minutes), minutes(after.expected_minutes)));
  }

  const oldList = list(before.candidates);
  const newList = list(after.candidates);
  const oldByKey = new Map(oldList.map((item) => [pairKey(item), item]));
  const newByKey = new Map(newList.map((item) => [pairKey(item), item]));
  const keysBefore = oldList.map(pairKey);
  const keysAfter = newList.map(pairKey);

  for (const keyOfPair of keysBefore) {
    if (!newByKey.has(keyOfPair)) lines.push({ mark: 'removed', text: `Удалён кандидат ${keyOfPair}` });
  }
  for (const keyOfPair of keysAfter) {
    if (!oldByKey.has(keyOfPair)) lines.push({ mark: 'added', text: `Добавлен кандидат ${keyOfPair}` });
  }

  const sameSet =
    keysBefore.length === keysAfter.length && keysBefore.every((item) => newByKey.has(item));
  if (sameSet && !same(keysBefore, keysAfter)) {
    lines.push({ mark: 'changed', text: `Порядок кандидатов: ${keysAfter.join(' → ')}` });
  }

  for (const [keyOfPair, oldCandidate] of oldByKey) {
    const newCandidate = newByKey.get(keyOfPair);
    if (newCandidate === undefined) continue;
    const diff = candidateDiff(oldCandidate, newCandidate);
    lines.push(...diff.lines);
    unchanged += diff.unchanged;
  }
  return { lines, unchanged };
}

const THRESHOLDS: { key: string; label: string }[] = [
  { key: 'max_weekly_percent', label: 'неделя' },
  { key: 'max_five_hour_percent', label: '5ч' },
  { key: 'max_monthly_percent', label: 'месяц' },
  { key: 'max_weekly_fable_percent', label: 'неделя Fable' }
];

function policyDiff(beforeValue: unknown, afterValue: unknown): DiffResult {
  const before = record(beforeValue) as Policy;
  const after = record(afterValue) as Policy;
  const lines: DiffLine[] = [];
  let unchanged = 0;

  const oldTasks = before.task_types ?? {};
  const newTasks = after.task_types ?? {};
  const taskKeys = [...new Set([...Object.keys(oldTasks), ...Object.keys(newTasks)])];
  for (const key of taskKeys) {
    const oldTask = oldTasks[key];
    const newTask = newTasks[key];
    if (!oldTask) {
      lines.push({ mark: 'added', text: `Тип задачи «${text(newTask?.title) || key}» добавлен` });
      for (const item of list(newTask?.candidates)) {
        lines.push({ mark: 'added', text: `Добавлен кандидат ${pairKey(item)}` });
      }
    } else if (!newTask) {
      lines.push({ mark: 'removed', text: `Тип задачи «${text(oldTask.title) || key}» удалён` });
      for (const item of list(oldTask.candidates)) {
        lines.push({ mark: 'removed', text: `Удалён кандидат ${pairKey(item)}` });
      }
    } else {
      const diff = taskDiff(oldTask, newTask);
      if (diff.lines.length) {
        lines.push({ mark: 'context', text: `Тип задачи «${text(newTask.title) || key}»` });
      }
      lines.push(...diff.lines);
      unchanged += diff.unchanged;
    }
  }

  const oldProviders = before.providers ?? {};
  const newProviders = after.providers ?? {};
  for (const name of [...new Set([...Object.keys(oldProviders), ...Object.keys(newProviders)])]) {
    const oldProvider = record(oldProviders[name]);
    const newProvider = record(newProviders[name]);
    if (!oldProviders[name]) {
      lines.push({ mark: 'added', text: `Порог провайдера ${name} добавлен` });
      continue;
    }
    if (!newProviders[name]) {
      lines.push({ mark: 'removed', text: `Порог провайдера ${name} удалён` });
      continue;
    }
    let providerChanged = false;
    for (const { key, label } of THRESHOLDS) {
      if (!same(oldProvider[key], newProvider[key])) {
        lines.push(changed(`Порог ${name} (${label})`, oldProvider[key], newProvider[key]));
        providerChanged = true;
      }
    }
    if (!providerChanged) unchanged += 1;
  }

  const oldParallel = before.max_parallel ?? {};
  const newParallel = after.max_parallel ?? {};
  for (const pair of [...new Set([...Object.keys(oldParallel), ...Object.keys(newParallel)])]) {
    if (!same(oldParallel[pair], newParallel[pair])) {
      lines.push(changed(`Пара ${pair}`, oldParallel[pair], newParallel[pair]));
    } else {
      unchanged += 1;
    }
  }

  const oldUnlimited = list(before.unlimited_models);
  const newUnlimited = list(after.unlimited_models);
  if (!same(oldUnlimited, newUnlimited)) {
    lines.push(changed('Неограниченные модели', oldUnlimited, newUnlimited));
  } else if (oldUnlimited.length) {
    unchanged += 1;
  }
  const beforeUnknown = before.unknown_limits ?? 'allow';
  const afterUnknown = after.unknown_limits ?? 'allow';
  if (beforeUnknown !== afterUnknown) {
    lines.push(
      changed(
        'Неизвестные лимиты',
        beforeUnknown === 'skip' ? 'пропускать' : 'разрешать',
        afterUnknown === 'skip' ? 'пропускать' : 'разрешать'
      )
    );
  } else {
    unchanged += 1;
  }

  return { lines, unchanged };
}

export function routingDiff(before: unknown, after: unknown): DiffResult {
  return policyDiff(before, after);
}

function entryKey(entry: WhitelistEntry): string {
  const harness = text(entry.harness) || '?';
  const model = text(entry.model);
  return `${harness}/${model === 'default' ? 'модель по умолчанию' : model || '?'}`;
}

export function whitelistDiff(before: unknown, after: unknown): DiffResult {
  const oldEntries = list(before) as WhitelistEntry[];
  const newEntries = list(after) as WhitelistEntry[];
  const keysOf = (entries: WhitelistEntry[]) => entries.map(entryKey);
  const oldKeys = keysOf(oldEntries);
  const newKeys = keysOf(newEntries);
  const lines: DiffLine[] = [];
  let unchanged = 0;
  for (const key of new Set([...oldKeys, ...newKeys])) {
    const was = oldKeys.filter((item) => item === key).length;
    const now = newKeys.filter((item) => item === key).length;
    for (let n = 0; n < Math.min(was, now); n += 1) unchanged += 1;
    for (let n = now; n < was; n += 1) lines.push({ mark: 'removed', text: `Удалена запись ${key}` });
    for (let n = was; n < now; n += 1) lines.push({ mark: 'added', text: `Добавлена запись ${key}` });
  }
  return { lines, unchanged };
}

const MAX_LCS_CELLS = 250_000;

function lcsDiff(before: string[], after: string[]): DiffLine[] {
  const n = before.length;
  const m = after.length;
  const width = m + 1;
  const table = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      table[i * width + j] =
        before[i] === after[j]
          ? table[(i + 1) * width + j + 1]! + 1
          : Math.max(table[(i + 1) * width + j]!, table[i * width + j + 1]!);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (before[i] === after[j]) {
      out.push({ mark: 'context', text: `  ${before[i]}` });
      i += 1;
      j += 1;
    } else if (table[(i + 1) * width + j]! >= table[i * width + j + 1]!) {
      out.push({ mark: 'removed', text: `− ${before[i]}` });
      i += 1;
    } else {
      out.push({ mark: 'added', text: `+ ${after[j]}` });
      j += 1;
    }
  }
  while (i < n) {
    out.push({ mark: 'removed', text: `− ${before[i]}` });
    i += 1;
  }
  while (j < m) {
    out.push({ mark: 'added', text: `+ ${after[j]}` });
    j += 1;
  }
  return out;
}

function plainDiff(before: string[], after: string[]): DiffLine[] {
  return [
    ...before.map((line): DiffLine => ({ mark: 'removed', text: `− ${line}` })),
    ...after.map((line): DiffLine => ({ mark: 'added', text: `+ ${line}` }))
  ];
}

/** Общий префикс и суффикс выносятся за скобки: длинный текст не даёт квадратичной таблицы. */
export function presetDiff(beforeText: unknown, afterText: unknown): DiffResult {
  const before = text(beforeText).length ? text(beforeText).split('\n') : [];
  const after = text(afterText).length ? text(afterText).split('\n') : [];
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start]) start += 1;
  let endBefore = before.length;
  let endAfter = after.length;
  while (endBefore > start && endAfter > start && before[endBefore - 1] === after[endAfter - 1]) {
    endBefore -= 1;
    endAfter -= 1;
  }
  const middleBefore = before.slice(start, endBefore);
  const middleAfter = after.slice(start, endAfter);
  const middle =
    middleBefore.length * middleAfter.length > MAX_LCS_CELLS
      ? plainDiff(middleBefore, middleAfter)
      : lcsDiff(middleBefore, middleAfter);
  const lines: DiffLine[] = [
    ...before.slice(0, start).map((line): DiffLine => ({ mark: 'context', text: `  ${line}` })),
    ...middle,
    ...before.slice(endBefore).map((line): DiffLine => ({ mark: 'context', text: `  ${line}` }))
  ];
  return { lines, unchanged: lines.filter((line) => line.mark === 'context').length };
}

const TITLES: Record<SettingKind, string> = {
  routing: 'Политика маршрутизации',
  whitelist: 'Белый список делегирования',
  preset: 'Пресет инструкций',
  unknown: 'Изменение настройки'
};

export function settingsChangeView(payload: unknown): SettingsChangeView {
  const top = record(payload);
  const raw = text(top.setting);
  const setting: SettingKind =
    raw === 'routing' || raw === 'whitelist' || raw === 'preset' ? raw : 'unknown';
  let diff: DiffResult = { lines: [], unchanged: 0 };
  if (setting === 'routing' || setting === 'unknown') diff = routingDiff(top.before, top.after);
  else if (setting === 'whitelist') diff = whitelistDiff(top.before, top.after);
  else if (setting === 'preset') diff = presetDiff(top.before, top.after);
  const presetName = setting === 'preset' ? text(top.title) : '';
  return {
    setting,
    title: setting === 'preset' && presetName ? `Инструкции: ${presetName}` : TITLES[setting],
    reason: text(top.reason),
    baseRevision: typeof top.base_revision === 'number' ? top.base_revision : 0,
    presetName: presetName || undefined,
    lines: diff.lines,
    unchanged: diff.unchanged
  };
}

/** Все исходы, которые хаб может записать в `request_closed`. */
export function settingsOutcome(reason?: string): SettingsOutcome {
  switch (reason) {
    case 'allowed':
    case 'applied':
      return { word: 'применено', tone: 'done' };
    case 'declined':
      return { word: 'отклонено', tone: 'danger' };
    case 'setting_changed':
      return { word: 'настройка изменилась', tone: 'warning' };
    case 'expired':
      return { word: 'истекло', tone: 'quiet' };
    case 'invalid_value':
      return { word: 'не применено: некорректное значение', tone: 'danger' };
    case 'invalid_payload':
      return { word: 'не применено: неверные данные решения', tone: 'danger' };
    case 'job_failed':
      return { word: 'не применено: сбой хаба', tone: 'danger' };
    case 'conversation_trashed':
      return { word: 'сессия удалена', tone: 'quiet' };
    case 'not_applied':
      return { word: 'не применено', tone: 'danger' };
    default:
      return { word: 'решение закрыто', tone: 'quiet' };
  }
}
