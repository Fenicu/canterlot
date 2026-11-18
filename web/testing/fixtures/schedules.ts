import type { components } from '../../src/lib/api/generated/openapi.js';
import type { FakeHubApi } from '../fake-hub';

type NewSchedule = components['schemas']['NewSchedule'];
type Schedule = components['schemas']['Schedule'];
type ScheduleRun = components['schemas']['ScheduleRun'];

export type ScheduleSeed = Partial<NewSchedule> & {
  id?: string;
  enabled?: boolean;
  revision?: number;
  next_fire_at?: string;
};

export const BASE_SCHEDULE: NewSchedule = {
  name: 'Разбор коммитов',
  cron: '0 9 * * 1-5',
  timezone: 'UTC',
  target: { kind: 'new_session', machine_id: 'workstation', workdir: '/srv/canterlot', route: { task_type: 'review' } },
  prompt: 'Посмотри коммиты за вчера',
  precheck: null,
  precheck_timeout_seconds: 60,
  catch_up_minutes: 30,
  overlap: 'skip',
  title_template: '{name} · {date}'
};

export function seedSchedule(api: FakeHubApi, overrides: ScheduleSeed = {}): Schedule {
  const { id, enabled, revision, next_fire_at, ...body } = overrides;
  const schedule = api.makeSchedule({ ...BASE_SCHEDULE, ...body }, id, revision ?? 1);
  if (enabled !== undefined) schedule.enabled = enabled;
  if (next_fire_at) schedule.next_fire_at = next_fire_at;
  api.schedules.set(schedule.id, schedule);
  api.scheduleRuns.set(schedule.id, api.scheduleRuns.get(schedule.id) ?? []);
  return schedule;
}

export function seedScheduleRun(api: FakeHubApi, scheduleId: string, overrides: Partial<ScheduleRun> = {}): ScheduleRun {
  const now = new Date().toISOString();
  const run: ScheduleRun = {
    id: overrides.id ?? api.id(),
    schedule_id: scheduleId,
    scheduled_at: now,
    claimed_revision: 1,
    state: 'started',
    created_at: now,
    started_at: now,
    delay_ms: 2400,
    session_id: null,
    input_id: null,
    manual: false,
    idempotency_key: api.id(),
    detail: {},
    ...overrides
  };
  api.scheduleRuns.set(scheduleId, [run, ...(api.scheduleRuns.get(scheduleId) ?? [])]);
  return run;
}
