import { describe, expect, it } from 'vitest';
import { SecretRequestsModel } from '#lib/stores/secretRequests.svelte.js';
import type { SecretRequestView } from '#lib/requests/secretRequest.js';

function view(patch: Partial<SecretRequestView>): SecretRequestView {
  return {
    id: 'sr-1',
    conversation_id: 'conv-1',
    name: 'Токен',
    purpose: 'деплой',
    state: 'open',
    created_at: '2026-11-16T11:55:00.000Z',
    updated_at: '2026-11-16T11:55:00.000Z',
    revision: 0,
    ...patch
  };
}

describe('stores/secretRequests', () => {
  it('an older response cannot overwrite a newer event', () => {
    const model = new SecretRequestsModel('conv-1');
    expect(model.apply(view({ state: 'delivered', revision: 5 }))).toBe(true);
    expect(model.apply(view({ state: 'revoking', revision: 4 }))).toBe(false);
    expect(model.get('sr-1')?.state).toBe('delivered');
    expect(model.apply(view({ state: 'revoking', revision: 6 }))).toBe(true);
    expect(model.get('sr-1')?.state).toBe('revoking');
  });

  it('an equal revision cannot overwrite an already stored state', () => {
    const model = new SecretRequestsModel('conv-1');
    expect(model.apply(view({ state: 'revoking', revision: 6 }))).toBe(true);
    expect(model.apply(view({ state: 'lost', revision: 6 }))).toBe(false);
    expect(model.get('sr-1')?.state).toBe('revoking');
  });

  it('a stale list snapshot keeps the newer event state', () => {
    const model = new SecretRequestsModel('conv-1');
    model.apply(view({ state: 'revoked', revision: 9 }));
    const applied = model.ingest([
      view({ state: 'delivered', revision: 3 }),
      view({ id: 'sr-2', state: 'open', revision: 1 })
    ]);
    expect(applied.map((item) => item.id)).toEqual(['sr-2']);
    expect(model.get('sr-1')?.state).toBe('revoked');
    expect(model.get('sr-2')?.state).toBe('open');
  });
});
