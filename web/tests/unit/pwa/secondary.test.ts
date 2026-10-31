import { describe, it, expect, vi } from 'vitest';
import { machineStatus, machineWarnings, diskView } from '#lib/screens/machines/model.js';
import { chartModel, usageRows, historyRange } from '#lib/screens/limits/model.js';
import { applyShare } from '#lib/screens/share/model.js';
import { MachinesStore } from '#lib/stores/machines.svelte.js';
import { FakeHub } from '../../../testing/fake-hub/index.js';
import type { components } from '#lib/api/generated/openapi.js';

type Usage = components['schemas']['UsageDay'];
const usage = (patch: Partial<Usage>): Usage => ({ day: '2026-10-29', conversation_id: 'abcdef12-0000', provider: 'claude', agent: 'main', model: 'opus', input_tokens: 100, output_tokens: 10, cache_read_tokens: 20, cache_write_tokens: 30, weekly_delta_permille: 20, overlapped_runs: 0, runs: 1, ...patch });

describe('Task 9b', () => {
  it('machine_status_labels_and_warnings_are_russian', () => {
    expect(['available', 'degraded', 'unavailable'].map(machineStatus)).toEqual(['В сети', 'Связь нестабильна', 'Не в сети']);
    expect(machineWarnings(['disk_usage_high', 'metrics_stale', 'limits_mismatch'])).toEqual(['Мало свободного места на диске', 'Данные устарели', 'Лимиты машины не совпадают с настройками хаба']);
  });
  it('disk_warning_at_the_85_percent_boundary', () => {
    expect(diskView({ label: 'data', free_bytes: 151, total_bytes: 1000 }, [], 85)).toMatchObject({ warn: false, label: 'Данные воркера' });
    expect(diskView({ label: 'root_0', free_bytes: 150, total_bytes: 1000 }, ['/srv'], 85)).toMatchObject({ warn: true, label: '/srv', pct: 85 });
    expect(diskView({ label: 'data', free_bytes: 0, total_bytes: 0 }, [], 85).pct).toBeNull();
  });
  it('limits_chart_model_shows_the_remaining_share_of_both_series', () => {
    const days = ['2026-10-28', '2026-10-29'];
    const series = [{ provider: 'claude', five_hour: [{ observed_at: '2026-10-29T12:00:00Z', used_permille: 340, used_pct: 34 }], weekly: [{ observed_at: '2026-10-29T12:00:00Z', used_permille: 960, used_pct: 96 }] }];
    const model = chartModel(series, days, 'UTC');
    expect(model[0]?.days[0]).toMatchObject({ five: null, weekly: null });
    expect(model[0]?.days[1]).toMatchObject({ five: 66, weekly: 4 });
  });
  it('usage_table_orders_by_weekly_delta_and_marks_estimates_and_agent_rows', () => {
    const rows = usageRows([usage({ weekly_delta_permille: 2, agent: 'agent-1', model: 'haiku', overlapped_runs: 1 }), usage({ weekly_delta_permille: 32 }), usage({ day: '2026-10-28' })], '2026-10-29', new Map([['abcdef12-0000', { title: 'Поток' }]]));
    expect(rows.map(r => r.delta)).toEqual([32, 2]);
    expect(rows[0]).toMatchObject({ title: 'Поток', input: 150, approximate: true });
    expect(rows[1]).toMatchObject({ detail: 'Субагент · haiku', approximate: true });
    expect(usageRows([usage({})], '2026-10-29', new Map())[0]?.title).toBe('Сессия abcdef12');
  });
  it('share_picker_attaches_files_and_appends_text_and_url_to_the_draft', async () => {
    const file = new File(['payload'], 'notes.txt');
    const model = { text: 'Черновик', attach: vi.fn(), errorMessage: null };
    const save = vi.fn(); const remove = vi.fn();
    await applyShare({ id: 'share', receivedAt: Date.now(), files: [file], title: 'Название', text: 'Текст', url: 'https://example.org/' }, model, save, remove);
    expect(model.attach).toHaveBeenCalledWith([file]);
    expect(model.text).toBe('Черновик\nНазвание\nТекст\nhttps://example.org/');
    expect(save).toHaveBeenCalledWith(model.text);
    expect(remove).toHaveBeenCalledAfter(save);
  });
});

it('share_attachment_error_keeps_the_draft_and_consumes_the_record_after_saving', async () => {
  const save = vi.fn(); const remove = vi.fn();
  const model = { text: '', errorMessage: 'Файл больше 100 МиБ', attach: vi.fn() };
  await applyShare({ id: 'share', receivedAt: Date.now(), files: [], text: 'Сохранить текст' }, model, save, remove);
  expect(save).toHaveBeenCalledWith('Сохранить текст');
  expect(remove).toHaveBeenCalledAfter(save);
});

it('share_does_not_consume_the_record_when_draft_persistence_fails', async () => {
  const remove = vi.fn();
  await expect(applyShare({ id: 'share', receivedAt: Date.now(), files: [], text: 'Текст' }, { text: '', errorMessage: null, attach: vi.fn() }, async () => { throw new Error('storage'); }, remove)).rejects.toThrow('storage');
  expect(remove).not.toHaveBeenCalled();
});

it('limits_calendar_and_snapshots_use_local_days_and_the_latest_point', () => {
  const series = [{ provider: 'codex', five_hour: [
    { observed_at: '2026-10-28T23:30:00Z', used_permille: 500, used_pct: 50 },
    { observed_at: '2026-10-28T21:30:00Z', used_permille: 200, used_pct: 20 }
  ], weekly: [] }];
  expect(chartModel(series, ['2026-10-29'], 'Europe/Moscow')[0]?.days[0]).toMatchObject({ five: 50, weekly: null });
});


it('limits_history_queries_rfc3339_midnights_for_seven_local_days', () => {
  const now = new Date(2026, 9, 29, 14, 30);
  const range = historyRange(now);
  const from = new Date(range.from); const to = new Date(range.to);
  expect(range.from).toContain('T'); expect(range.to).toContain('T');
  expect([from.getDate(), from.getHours(), to.getDate(), to.getHours()]).toEqual([23, 0, 30, 0]);
});


it('fresh_machine_metrics_clear_the_snapshot_stale_warning', () => {
  const hub = new FakeHub();
  const id = '00000000-0000-4000-8000-000000000001';
  const store = new MachinesStore({ connection: {} as never, clock: { now: Date.now }, api: { snapshot: async () => ({ cursor: '0', data: { items: [] } }), events: async () => ({ events: [], head: '0' }) } });
  store.applier.applySnapshot({ items: [{ id, name: 'Машина', os: null, availability: 'available', warnings: ['metrics_stale'], metrics: null, harnesses: [], profiles: [] }] }, '0');
  const event = hub.append('machines', { kind: 'machine_metrics', payload: { machine_id: id, machine_time_ms: Date.now(), cpu_cores: 8, cpu_permille: 240, mem_total_bytes: 1000, mem_available_bytes: 600, load1_centi: 100, load5_centi: 100, load15_centi: 100, disks: [], executions: [] } });
  store.applier.applyEvents([{ event, overlap: false }]);
  expect(store.metricsStale(id)).toBe(false);
  expect(store.items.get(id)?.metrics?.mem_total_bytes).toBe(1000);
});
