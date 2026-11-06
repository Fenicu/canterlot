import { describe, expect, test } from 'vitest';
import {
  MAX_CANDIDATES,
  MAX_TASK_TYPES,
  addCandidate,
  addTask,
  allowedByWhitelist,
  fromPolicy,
  isDirty,
  issuesAt,
  move,
  moveCandidate,
  removeCandidate,
  toPolicy,
  validate,
  type Draft,
  type Policy
} from '#lib/screens/settings/routingPolicy.js';

const policy: Policy = {
  task_types: {
    bugfix: {
      title: 'Исправление',
      isolated: true,
      candidates: [
        { harness: 'opencode', model: 'opencode-go/deepseek-v4.1-flash', effort: 'high', nudge: { max_nudges: 2 }, watchdog_minutes: 90, exclude_workdirs: [] },
        { harness: 'opencode', model: 'opencode-go/muse', nudge: true, watchdog_minutes: 120, exclude_workdirs: ['/srv/prod'] },
        { harness: 'claude_code', model: 'opus', effort: 'high', nudge: false, exclude_workdirs: [] }
      ]
    }
  },
  providers: { codex: { max_weekly_percent: 80, max_five_hour_percent: null, max_monthly_percent: null } },
  unlimited_models: ['opencode-go/beam-free'],
  max_parallel: { 'codex/default': 1 },
  unknown_limits: 'allow'
};

function draft(): Draft {
  return fromPolicy(policy);
}

describe('routing policy editor model', () => {
  test('round trip keeps the saved policy and the draft is clean', () => {
    const d = draft();
    expect(d.tasks[0]!.candidates.map((c) => c.nudge)).toEqual(['max', 'on', 'off']);
    expect(d.tasks[0]!.candidates[0]!.maxNudges).toBe(2);
    expect(d.providers.map((p) => p.provider)).toEqual(['claude', 'codex', 'opencode']);
    expect(isDirty(d, policy)).toBe(false);
    const saved = toPolicy(d);
    expect(saved.task_types!.bugfix!.candidates[0]).toMatchObject({ nudge: { max_nudges: 2 }, watchdog_minutes: 90, effort: 'high' });
    expect(saved.task_types!.bugfix!.candidates[1]).toMatchObject({ nudge: true, exclude_workdirs: ['/srv/prod'] });
    expect(saved.providers).toEqual({ codex: { max_weekly_percent: 80, max_five_hour_percent: null, max_monthly_percent: null } });
  });

  test('reorder makes the draft dirty and moving back makes it clean', () => {
    const d = draft();
    const uidOf = d.tasks[0]!.uid;
    const moved = moveCandidate(d, uidOf, 2, 0);
    expect(moved.tasks[0]!.candidates.map((c) => c.model)).toEqual(['opus', 'opencode-go/deepseek-v4.1-flash', 'opencode-go/muse']);
    expect(isDirty(moved, policy)).toBe(true);
    expect(isDirty(moveCandidate(moved, uidOf, 0, 2), policy)).toBe(false);
  });

  test('move ignores positions outside the list', () => {
    expect(move([1, 2, 3], 0, -1)).toEqual([1, 2, 3]);
    expect(move([1, 2, 3], 2, 3)).toEqual([1, 2, 3]);
    expect(move([1, 2, 3], 0, 1)).toEqual([2, 1, 3]);
  });

  test('editing a threshold or the unknown-limits rule is a change', () => {
    const d = draft();
    d.providers[0]!.weekly = 90;
    expect(isDirty(d, policy)).toBe(true);
    const e = draft();
    e.unknownLimits = 'skip';
    expect(isDirty(e, policy)).toBe(true);
  });

  test('validation messages are Russian and point at the item', () => {
    const d = draft();
    const task = d.tasks[0]!;
    task.key = 'Bad Key';
    task.title = ' ';
    task.candidates[0]!.watchdog = 3;
    task.candidates[1]!.exclude = ['relative/path'];
    task.candidates[2]!.nudge = 'max';
    task.candidates[2]!.maxNudges = 11;
    d.providers[1]!.weekly = 120;
    const issues = validate(d);
    expect(issuesAt(issues, `task:${task.uid}`)).toEqual([
      'Ключ: строчные латинские буквы, цифры и «_», до 32 знаков',
      'Нужно название'
    ]);
    expect(issuesAt(issues, `candidate:${task.candidates[0]!.uid}`)).toEqual(['Сторож: от 5 до 1440 минут']);
    expect(issuesAt(issues, `candidate:${task.candidates[1]!.uid}`)).toEqual(['Каталог «relative/path» должен начинаться с /']);
    expect(issuesAt(issues, `candidate:${task.candidates[2]!.uid}`)).toEqual(['Толчков: от 1 до 10']);
    expect(issuesAt(issues, 'provider:codex')).toEqual(['Порог: от 0 до 100 %']);
  });

  test('duplicate keys and empty candidate lists are rejected', () => {
    let d = draft();
    const added = addTask(d, { harness: 'codex', model: 'default' });
    if (typeof added === 'string') throw new Error(added);
    d = added;
    d.tasks[1]!.key = 'bugfix';
    d = removeCandidate(d, d.tasks[1]!.uid, d.tasks[1]!.candidates[0]!.uid);
    const messages = issuesAt(validate(d), `task:${d.tasks[1]!.uid}`);
    expect(messages).toContain('Ключ «bugfix» уже занят');
    expect(messages).toContain('Нужен хотя бы один кандидат');
  });

  test('limits: 32 task types and 8 candidates', () => {
    let d = draft();
    for (let i = d.tasks.length; i < MAX_TASK_TYPES; i += 1) {
      const next = addTask(d, { harness: 'codex', model: 'default' });
      if (typeof next === 'string') throw new Error(next);
      d = next;
    }
    expect(addTask(d, { harness: 'codex', model: 'default' })).toBe('Не больше 32 типов задач');
    let e = draft();
    for (let i = e.tasks[0]!.candidates.length; i < MAX_CANDIDATES; i += 1) {
      const next = addCandidate(e, e.tasks[0]!.uid, { harness: 'codex', model: 'default' });
      if (typeof next === 'string') throw new Error(next);
      e = next;
    }
    expect(addCandidate(e, e.tasks[0]!.uid, { harness: 'codex', model: 'default' })).toBe('Не больше 8 кандидатов');
  });

  test('whitelist check mirrors the hub: default entry allows the first model', () => {
    const entries = [{ harness: 'codex', model: 'default' }, { harness: 'opencode', model: 'opencode-go/muse' }];
    expect(allowedByWhitelist(entries, 'codex', 'default')).toBe(true);
    expect(allowedByWhitelist(entries, 'codex', 'gpt-6', 'gpt-6')).toBe(true);
    expect(allowedByWhitelist(entries, 'codex', 'gpt-5', 'gpt-6')).toBe(false);
    expect(allowedByWhitelist(entries, 'opencode', 'opencode-go/muse')).toBe(true);
    expect(allowedByWhitelist(entries, 'claude_code', 'opus')).toBe(false);
  });

  test('parallel pairs must look like harness/model and be unique', () => {
    const d = draft();
    d.parallel.push({ uid: 'x1', pair: 'codex/default', limit: 2 }, { uid: 'x2', pair: 'nonsense', limit: 0 });
    const issues = validate(d);
    expect(issuesAt(issues, 'parallel:x1')).toEqual(['Пара «codex/default» уже есть']);
    expect(issuesAt(issues, 'parallel:x2')).toEqual(['Пара в виде агент/модель', 'Не меньше одной сессии']);
  });
});
