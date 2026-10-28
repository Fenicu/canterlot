import type { ConnectionStatus } from './connection';
import type { ScopeStatus } from './scope';

export function connectionState(now: number): ConnectionStatus {
  const state: ConnectionStatus = $state({ phase: 'connecting', since: now, lastFrameAt: now, attempt: 0, resyncs: 0 });
  return state;
}
export function scopeState(): { cursor: string; status: ScopeStatus } {
  const state: { cursor: string; status: ScopeStatus } = $state({ cursor: '0', status: 'idle' });
  return state;
}
