import { describe, expect, it } from 'vitest';
import {
  presetDiff,
  routingDiff,
  settingsChangeView,
  settingsOutcome,
  whitelistDiff,
  type Policy,
  type WhitelistEntry
} from '#lib/requests/settingsChangeView.js';

const candidate = (harness: string, model: string, extra: Record<string, unknown> = {}) => ({
  harness,
  model,
  ...extra
});

const task = (title: string, candidates: unknown[], extra: Record<string, unknown> = {}) => ({
  title,
  candidates,
  ...extra
});

const policy = (taskTypes: Record<string, unknown>, extra: Record<string, unknown> = {}): Policy =>
  ({
    task_types: taskTypes,
    providers: {},
    max_parallel: {},
    unlimited_models: [],
    unknown_limits: 'allow',
    ...extra
  }) as Policy;

describe('routingDiff', () => {
  it('пустая политика → политика с типом задачи и кандидатом', () => {
    const result = routingDiff(null, policy({ one: task('Тип один', [candidate('h1', 'm1')]) }));
    expect(result.lines.map((line) => line.text)).toEqual([
      'Тип задачи «Тип один» добавлен',
      'Добавлен кандидат h1/m1'
    ]);
    expect(result.lines.every((line) => line.mark === 'added')).toBe(true);
  });

  it('политика → пустая политика', () => {
    const result = routingDiff(policy({ one: task('Тип один', [candidate('h1', 'm1')]) }), null);
    expect(result.lines.map((line) => line.text)).toEqual([
      'Тип задачи «Тип один» удалён',
      'Удалён кандидат h1/m1'
    ]);
    expect(result.lines.every((line) => line.mark === 'removed')).toBe(true);
  });

  it('только перестановка кандидатов: одна строка о новом порядке', () => {
    const before = policy({ one: task('Тип один', [candidate('h1', 'm1'), candidate('h2', 'm2')]) });
    const after = policy({ one: task('Тип один', [candidate('h2', 'm2'), candidate('h1', 'm1')]) });
    const result = routingDiff(before, after);
    expect(result.lines).toHaveLength(1);
    expect(result.lines[0]).toMatchObject({ mark: 'changed' });
    expect(result.lines[0]!.text).toContain('Порядок кандидатов');
    expect(result.lines[0]!.text).toContain('h2/m2 → h1/m1');
  });

  it('только смена усилия: human-readable старое и новое значение', () => {
    const before = policy({ one: task('Тип один', [candidate('h1', 'm1', { effort: 'low' })]) });
    const after = policy({ one: task('Тип один', [candidate('h1', 'm1', { effort: 'high' })]) });
    const result = routingDiff(before, after);
    expect(result.lines).toHaveLength(1);
    expect(result.lines[0]!.text).toContain('Усилие');
    expect(result.lines[0]!.text).toContain('низкое');
    expect(result.lines[0]!.text).toContain('высокое');
  });

  it('автопинг, сторож и порог провайдера описаны по отдельности', () => {
    const before = policy(
      { one: task('Тип один', [candidate('h1', 'm1', { nudge: false, watchdog_minutes: 30 })]) },
      { providers: { p1: { max_weekly_percent: 70 } } }
    );
    const after = policy(
      { one: task('Тип один', [candidate('h1', 'm1', { nudge: { max_nudges: 5 }, watchdog_minutes: 45 })]) },
      { providers: { p1: { max_weekly_percent: 80 } } }
    );
    const texts = routingDiff(before, after).lines.map((line) => line.text);
    expect(texts.some((text) => text.includes('Автопинг') && text.includes('5'))).toBe(true);
    expect(texts.some((text) => text.includes('Сторож') && text.includes('30') && text.includes('45'))).toBe(true);
    expect(texts.some((text) => text.includes('Порог') && text.includes('70') && text.includes('80'))).toBe(true);
  });

  it('тип задачи добавлен и удалён', () => {
    const before = policy({ gone: task('Удалённый', [candidate('h1', 'm1')]) });
    const after = policy({ fresh: task('Новый', [candidate('h2', 'm2')]) });
    const texts = routingDiff(before, after).lines.map((line) => line.text);
    expect(texts).toContain('Тип задачи «Новый» добавлен');
    expect(texts).toContain('Тип задачи «Удалённый» удалён');
  });

  it('идентичные политики не дают изменений, но считают без изменений', () => {
    const value = policy({ one: task('Тип один', [candidate('h1', 'm1', { effort: 'medium' })]) });
    const result = routingDiff(value, value);
    expect(result.lines).toEqual([]);
    expect(result.unchanged).toBeGreaterThan(0);
  });

  it('неизвестные поля не ломают разбор', () => {
    const result = routingDiff({ task_types: { one: { title: 'Тип один', candidates: [{}] } } }, null);
    expect(result.lines.length).toBeGreaterThan(0);
  });
});

describe('whitelistDiff', () => {
  it('записи добавлены и удалены', () => {
    const before: WhitelistEntry[] = [{ harness: 'h1', model: 'm1' }];
    const after: WhitelistEntry[] = [{ harness: 'h2', model: 'm2' }];
    expect(whitelistDiff(before, after).lines.map((line) => line.text)).toEqual([
      'Удалена запись h1/m1',
      'Добавлена запись h2/m2'
    ]);
  });

  it('пустой список → записи', () => {
    expect(whitelistDiff(null, [{ harness: 'h1', model: 'default' }]).lines.map((line) => line.text)).toEqual([
      'Добавлена запись h1/модель по умолчанию'
    ]);
  });

  it('идентичные списки не дают изменений', () => {
    const entries = [{ harness: 'h1', model: 'm1' }];
    expect(whitelistDiff(entries, entries).lines).toEqual([]);
  });
});

describe('presetDiff', () => {
  it('добавленные и удалённые строки отмечены знаком в тексте', () => {
    const result = presetDiff('первая\nвторая', 'первая\nтретья\nчетвёртая');
    expect(result.lines.map((line) => line.mark)).toEqual(['context', 'removed', 'added', 'added']);
    expect(result.lines.map((line) => line.text)).toEqual([
      '  первая',
      '− вторая',
      '+ третья',
      '+ четвёртая'
    ]);
  });

  it('пустой текст → непустой', () => {
    expect(presetDiff('', 'первая строка\nвторая строка').lines.map((line) => line.mark)).toEqual(['added', 'added']);
  });

  it('непустой текст → пустой', () => {
    expect(presetDiff('первая\nвторая', '').lines.map((line) => line.mark)).toEqual(['removed', 'removed']);
  });

  it('идентичный текст — только строки без изменений', () => {
    const result = presetDiff('один\nдва', 'один\nдва');
    expect(result.lines.every((line) => line.mark === 'context')).toBe(true);
    expect(result.lines).toHaveLength(2);
  });

  it('очень длинный текст обрабатывается без квадратичного взрыва', () => {
    const before = Array.from({ length: 4000 }, (_, n) => `строка ${n}`).join('\n');
    const after = Array.from({ length: 4000 }, (_, n) => (n === 2000 ? 'новая строка' : `строка ${n}`)).join('\n');
    const result = presetDiff(before, after);
    expect(result.lines).toHaveLength(4001);
    expect(result.lines.filter((line) => line.mark === 'added')).toHaveLength(1);
    expect(result.lines.filter((line) => line.mark === 'removed')).toHaveLength(1);
  });
});

describe('settingsChangeView', () => {
  it('разбирает три вида предложения', () => {
    const base = { reason: 'Причина', base_revision: 7 };
    expect(settingsChangeView({ ...base, setting: 'routing', before: null, after: policy({}) }).setting).toBe('routing');
    expect(settingsChangeView({ ...base, setting: 'whitelist', before: [], after: [{ harness: 'h1', model: 'm1' }] }).setting).toBe('whitelist');
    const preset = settingsChangeView({
      ...base,
      setting: 'preset',
      preset_id: 'p1',
      title: 'Название пресета',
      before: 'старый текст',
      after: 'новый текст'
    });
    expect(preset.setting).toBe('preset');
    expect(preset.title).toContain('Название пресета');
    expect(preset.presetName).toBe('Название пресета');
    expect(preset.baseRevision).toBe(7);
    expect(preset.reason).toBe('Причина');
  });

  it('неизвестный вид и битые данные не ломают карточку', () => {
    const view = settingsChangeView({ setting: 'something', reason: 'Причина', base_revision: 1 });
    expect(view.lines).toEqual([]);
    expect(view.title).toBeTruthy();
    expect(settingsChangeView(null).lines).toEqual([]);
  });
});

describe('settingsOutcome', () => {
  it('покрывает все исходы хаба', () => {
    expect(settingsOutcome('allowed')).toMatchObject({ word: 'применено', tone: 'done' });
    expect(settingsOutcome('applied')).toMatchObject({ word: 'применено', tone: 'done' });
    expect(settingsOutcome('declined')).toMatchObject({ word: 'отклонено', tone: 'danger' });
    expect(settingsOutcome('setting_changed')).toMatchObject({ word: 'настройка изменилась', tone: 'warning' });
    expect(settingsOutcome('expired')).toMatchObject({ word: 'истекло', tone: 'quiet' });
    expect(settingsOutcome('invalid_value').word).toContain('некоррект');
    expect(settingsOutcome('invalid_payload').word).toContain('неверн');
    expect(settingsOutcome('job_failed').word).toContain('сбой');
    expect(settingsOutcome('conversation_trashed').word).toContain('удален');
    expect(settingsOutcome('not_applied').word).toContain('не применено');
    expect(settingsOutcome('something_new').word).toBeTruthy();
    expect(settingsOutcome(undefined).word).toBeTruthy();
  });
});
