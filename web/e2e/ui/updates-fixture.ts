import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import type { FakeHub } from '../../testing/fake-hub';
import type { FakeUpdate } from '../../testing/fake-hub/api';

export const UPDATE_COMMAND = 'npm i -g @openai/codex@latest';

const BASE = Date.UTC(2026, 10, 18, 10, 0, 0);

export function makeUpdate(
  number: number,
  kind: string,
  minutesAgo: number,
  payload: Record<string, unknown>,
  extras: Partial<FakeUpdate> = {}
): FakeUpdate {
  return {
    id: `00000000-0000-4000-8000-${String(number).padStart(12, '0')}`,
    kind,
    machine: 'workstation',
    harness: 'codex',
    created_at: new Date(BASE - minutesAgo * 60_000).toISOString(),
    payload,
    ...extras
  };
}

/** Все пять видов в одном наборе: строки ленты и баннера берутся из одного текста. */
export const FEED: FakeUpdate[] = [
  makeUpdate(1, 'version_available', 1, {
    installed_version: '0.169', latest_version: '0.170', min_version: '0.150', update_command: UPDATE_COMMAND
  }, { update_command: UPDATE_COMMAND }),
  makeUpdate(2, 'below_minimum', 2, {
    installed_version: '2.2.4', latest_version: '2.3.0', min_version: '2.2.6'
  }, { harness: 'opencode' }),
  makeUpdate(3, 'cli_updated', 3, { from: '2.1.311', to: '2.1.330' }, { harness: 'claude_code' }),
  makeUpdate(4, 'models_added', 4, { models: ['claude-fable-5-5', 'gpt-6.1-sol'] }),
  makeUpdate(5, 'models_removed', 5, { models: ['gpt-6.1-sol'] })
];

/** Живое событие приходит в области машин; лента и баннер не делают опроса. */
export function emitUpdate(hub: FakeHub, api: { updates: FakeUpdate[] }, update: FakeUpdate): void {
  if (!api.updates.some((known) => known.id === update.id)) api.updates.unshift(update);
  hub.append('machines', {
    kind: 'machine_update',
    payload: {
      id: update.id,
      kind: update.kind,
      machine: update.machine,
      harness: update.harness,
      created_at: update.created_at,
      payload: update.payload
    }
  });
  hub.flush();
}

export function shot(name: string, project: string): string {
  const path = `test-results/updates/${name}-${project}.png`;
  mkdirSync(dirname(path), { recursive: true });
  return path;
}
