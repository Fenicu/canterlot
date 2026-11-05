import { describe, it, expect, vi } from 'vitest';
import { machineStatus, machineWarnings, diskView, diskLimits, lowDiskNotice } from '#lib/screens/machines/model.js';
import { historyCharts, usageRows, historyRange } from '#lib/screens/limits/model.js';
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
  it('disk_free_bytes_choose_warning_and_danger_tones_and_the_free_text', () => {
    const limits = diskLimits({ disk_warn_free_gb: 30, disk_critical_free_gb: 10 });
    expect(diskView({ label: 'data', free_bytes: 31_000_000_000, total_bytes: 295_000_000_000 }, [], limits)).toMatchObject({ tone: 'normal', label: 'Данные воркера', text: 'Свободно 31 ГБ из 295' });
    expect(diskView({ label: 'data', free_bytes: 24_000_000_000, total_bytes: 295_000_000_000 }, [], limits)).toMatchObject({ tone: 'warning', warn: true, text: 'Свободно 24 ГБ из 295' });
    expect(diskView({ label: 'root_0', free_bytes: 8_000_000_000, total_bytes: 295_000_000_000 }, ['/srv'], limits)).toMatchObject({ tone: 'danger', label: '/srv' });
    expect(diskView({ label: 'data', free_bytes: 0, total_bytes: 0 }, [], limits).pct).toBeNull();
  });
  it('low_disk_notice_names_the_worst_machine_and_uses_hysteresis', () => {
    const limits = diskLimits({ disk_warn_free_gb: 30, disk_critical_free_gb: 10 });
    const machine = (id: string, name: string, free: number) => ({ id, name, metrics: { disks: [{ label: 'data', total_bytes: 295_000_000_000, free_bytes: free }] } });
    const first = lowDiskNotice([machine('dev-vm', 'dev-vm', 24_000_000_000)], limits, []);
    expect(first.notice).toEqual({ text: 'На dev-vm мало места: свободно 24 ГБ', tone: 'warning' });
    expect(first.active).toEqual(['dev-vm']);
    // Выше порога, но ниже порога с запасом: строка ещё держится.
    expect(lowDiskNotice([machine('dev-vm', 'dev-vm', 34_000_000_000)], limits, first.active).notice?.text).toBe('На dev-vm мало места: свободно 34 ГБ');
    // Возврат с запасом убирает строку.
    expect(lowDiskNotice([machine('dev-vm', 'dev-vm', 36_000_000_000)], limits, first.active).notice).toBeNull();
    // Критический уровень даёт красный тон.
    expect(lowDiskNotice([machine('dev-vm', 'dev-vm', 8_000_000_000)], limits, []).notice?.tone).toBe('danger');
  });
  it('limits_history_shows_the_remaining_weekly_share_and_ignores_five_hours', () => {
    const days = ['2026-10-28', '2026-10-29'];
    const series = [{ provider: 'claude', five_hour: [{ observed_at: '2026-10-29T12:00:00Z', used_permille: 340, used_pct: 34 }], weekly: [{ observed_at: '2026-10-29T12:00:00Z', used_permille: 960, used_pct: 96 }] }];
    const model = historyCharts(series, days, 'UTC');
    expect(model[0]?.days[0]).toMatchObject({ value: null, text: '' });
    expect(model[0]?.days[1]).toMatchObject({ value: 4, text: '4%' });
  });
  it('usage_table_orders_by_weekly_delta_and_marks_estimates_and_agent_rows', () => {
    const rows = usageRows([usage({ weekly_delta_permille: 2, agent: 'agent-1', model: 'haiku', overlapped_runs: 1 }), usage({ weekly_delta_permille: 32 }), usage({ day: '2026-10-28' })], '2026-10-29', new Map([['abcdef12-0000', { title: 'Поток' }]]), new Set(['claude']));
    expect(rows.map(r => r.delta)).toEqual([32, 2]);
    expect(rows[0]).toMatchObject({ title: 'Поток', input: 150, approximate: true });
    expect(rows[1]).toMatchObject({ detail: 'Субагент · haiku', approximate: true });
    expect(usageRows([usage({})], '2026-10-29', new Map(), new Set(['claude']))[0]?.title).toBe('Сессия abcdef12');
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
  const series = [{ provider: 'codex', five_hour: [], weekly: [
    { observed_at: '2026-10-28T23:30:00Z', used_permille: 500, used_pct: 50 },
    { observed_at: '2026-10-28T21:30:00Z', used_permille: 200, used_pct: 20 }
  ] }];
  expect(historyCharts(series, ['2026-10-29'], 'Europe/Moscow')[0]?.days[0]).toMatchObject({ value: 50, text: '50%' });
});


it('limits_history_queries_rfc3339_midnights_for_seven_local_days', () => {
  const now = new Date(2026, 9, 29, 14, 30);
  const range = historyRange(now);
  const from = new Date(range.from); const to = new Date(range.to);
  expect(range.from).toContain('T'); expect(range.to).toContain('T');
  expect([from.getDate(), from.getHours(), to.getDate(), to.getHours()]).toEqual([23, 0, 30, 0]);
});


it('journal_stops_name_their_scope_and_ask_for_an_update', () => {
  expect(machineWarnings(['cj_journal_stopped', 'sj_journal_stopped'])).toEqual([
    'Хаб не принимает журнал машины: все её сессии и команды стоят, пока не обновлён хаб или воркер',
    'Хаб не принимает журнал сессии: её события не доставляются, пока не обновлён хаб или воркер'
  ]);
});
it('journal_stop_event_replaces_only_journal_warnings', () => {
  const id = '00000000-0000-4000-8000-000000000001';
  const store = new MachinesStore({ api: { snapshot: vi.fn(), events: vi.fn() }, connection: {} as never, clock: { now: () => 0 } });
  store.applier.applySnapshot({ items: [{ id, name: 'Машина', os: null, availability: 'available', warnings: ['limits_mismatch', 'sj_journal_stopped'], metrics: null, harnesses: [] }] }, '0');
  const stops = (stopped: string[]) => store.applier.applyEvents([{ event: { kind: 'machine_journal_stops_changed', committed_at: '2026-10-31T00:00:00Z', payload: { machine_id: id, stopped } } }] as never);
  stops(['cj']);
  expect(store.items.get(id)?.warnings).toEqual(['limits_mismatch', 'cj_journal_stopped']);
  stops([]);
  expect(store.items.get(id)?.warnings).toEqual(['limits_mismatch']);
  expect(store.unknownKinds).toBe(0);
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
