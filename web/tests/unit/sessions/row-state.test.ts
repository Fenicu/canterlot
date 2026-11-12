import { describe, expect, it } from 'vitest';
import { stateMark } from '#lib/screens/sessions/rowState.js';
import type { Activity } from '#lib/screens/sessions/activityText.js';

const turn = '2026-11-12T11:00:00.000Z';
const idle = (evidence: Activity['evidence']): Activity => ({ state: 'idle', evidence });

describe('stateMark', () => {
  it('rings the avatar in accent while a turn runs', () => {
    expect(stateMark({ state: 'working', evidence: { phase: 'generating' } })).toEqual({ state: 'running', word: 'Идёт ход', halo: '--accent', dashed: false, dot: null, dim: false, strong: false });
    expect(stateMark({ state: 'working', evidence: { phase: 'starting' } }).state).toBe('running');
    expect(stateMark(idle({ last_turn_at: turn, background_work: 2, process_alive: true })).halo).toBe('--accent');
  });

  it('rings amber and makes the word bold when the owner is awaited', () => {
    expect(stateMark({ state: 'waiting_for_user', evidence: {} })).toMatchObject({ state: 'waiting', halo: '--warning', dashed: false, strong: true, dim: false });
  });

  it('rings red for failed and stalled sessions', () => {
    expect(stateMark({ state: 'failed', evidence: { reason: 'process_exited' } })).toMatchObject({ state: 'failed', halo: '--danger', strong: false });
    expect(stateMark({ state: 'stalled', evidence: {} })).toMatchObject({ state: 'stalled', halo: '--danger' });
  });

  it('draws a dashed amber ring while limits hold the session', () => {
    expect(stateMark({ state: 'quota_paused', evidence: { reason: 'quota_exhausted' } })).toMatchObject({ state: 'paused', halo: '--warning', dashed: true });
  });

  it('puts a green dot on idle with a live process', () => {
    expect(stateMark(idle({ last_turn_at: turn, process_alive: true }))).toEqual({ state: 'live', word: 'На связи', halo: null, dashed: false, dot: '--success', dim: false, strong: false });
  });

  it('greys out idle without a process like a stopped session', () => {
    expect(stateMark(idle({ last_turn_at: turn, process_alive: false }))).toEqual({ state: 'dormant', word: 'Процесс не запущен', halo: null, dashed: false, dot: null, dim: true, strong: false });
    expect(stateMark(idle({ last_turn_at: null, process_alive: false })).dim).toBe(true);
    expect(stateMark({ state: 'stopped', evidence: { outcome: 'user_stop' } })).toEqual({ state: 'stopped', word: 'Остановлена', halo: null, dashed: false, dot: null, dim: true, strong: false });
  });

  it('falls back to the old reading when the hub does not send process_alive', () => {
    expect(stateMark(idle({ last_turn_at: turn }))).toMatchObject({ state: 'live', dot: '--success', dim: false });
    expect(stateMark(idle({ last_turn_at: null }))).toMatchObject({ state: 'fresh', dot: '--muted', dim: false });
    expect(stateMark({ state: 'idle' })).toMatchObject({ state: 'fresh', dim: false });
  });
});
