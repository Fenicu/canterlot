import 'fake-indexeddb/auto';
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { RequestsModel } from '#lib/stores/requests.svelte.js';
import { closeOpenJournals, OpJournal, type JournalEnv, type Op } from '#lib/ops/journal.js';
import { openDb, purgeAllStores } from '#lib/storage/idb.js';
import { ApiError, NetworkError, type ErrorCode, type ErrorScope, type ErrorDetails } from '#lib/api/http.js';
import type { DecisionBody } from '#lib/api/client.js';
import type { HubEvent } from '#lib/sync/wire.js';

class TestEnv implements JournalEnv {
  nowMs = 1000;
  isOnline = true;
  expired = false;
  scheduled: { at: number; fn: () => void }[] = [];

  now = (): number => this.nowMs;
  online = (): boolean => this.isOnline;
  authExpired = (): boolean => this.expired;
  schedule = (fn: () => void, ms: number): (() => void) => {
    const item = { at: this.nowMs + ms, fn };
    this.scheduled.push(item);
    return () => {
      const idx = this.scheduled.indexOf(item);
      if (idx >= 0) this.scheduled.splice(idx, 1);
    };
  };

  async advance(ms: number): Promise<void> {
    this.nowMs += ms;
    const due = this.scheduled.filter((s) => s.at <= this.nowMs);
    for (const item of due) {
      const idx = this.scheduled.indexOf(item);
      if (idx >= 0) this.scheduled.splice(idx, 1);
      item.fn();
    }
  }
}

describe('RequestsModel', () => {
  beforeEach(async () => {
    closeOpenJournals();
    await purgeAllStores();
  });

  afterEach(() => {
    closeOpenJournals();
  });

  it('requests_store_indexes_incoming_request_opened_events', () => {
    const model = new RequestsModel('conv-1');
    const openedEvent = {
      v: 1,
      id: 'ev-1',
      scope: 'conversation:conv-1',
      scope_seq: '1',
      scope_seq_from: '1',
      committed_at: '2026-10-29T12:00:00Z',
      committed_at_from: '2026-10-29T12:00:00Z',
      conversation_id: 'conv-1',
      execution_id: 'exec-1',
      correlation_id: 'corr-1',
      kind: 'request_opened',
      payload: {
        request_id: 'req-42',
        kind: 'question',
        payload: {
          questions: [
            { id: 'q1', text: 'Apply migrations?', options: ['Yes', 'No'] }
          ]
        }
      }
    } as unknown as HubEvent;

    model.applyOpened(openedEvent);

    expect(model.items.has('req-42')).toBe(true);
    const item = model.items.get('req-42')!;
    expect(item.id).toBe('req-42');
    expect(item.conversationId).toBe('conv-1');
    expect(item.kind).toBe('question');
    expect(item.state).toBe('open');
    const p = item.payload as { questions: { text: string }[] };
    expect(p.questions[0]?.text).toBe('Apply migrations?');
  });

  it('requests_store_populates_from_conversation_snapshot', () => {
    const model = new RequestsModel('conv-1');
    const snapshotRequests = [
      {
        id: 'req-10',
        conversation_id: 'conv-1',
        kind: 'permission',
        state: 'open',
        request_payload: {
          command: 'cargo test',
          description: 'Run unit tests'
        }
      },
      {
        id: 'req-20',
        conversation_id: 'conv-1',
        kind: 'question',
        state: 'decision_saved',
        request_payload: { questions: [{ id: 'q0', text: 'Which model?' }] },
        decision_payload: { answers: { q0: 'claude-3-5-sonnet' } }
      }
    ];

    model.ingestSnapshot(snapshotRequests);

    expect(model.items.size).toBe(2);
    const perm = model.items.get('req-10')!;
    expect(perm.kind).toBe('permission');
    expect(perm.state).toBe('open');

    const quest = model.items.get('req-20')!;
    expect(quest.kind).toBe('question');
    expect(quest.state).toBe('saved');
    const winner = quest.winnerDecision as { answers: Record<string, string> };
    expect(winner.answers.q0).toBe('claude-3-5-sonnet');
  });

  it('submitting_decision_transitions_to_sending_then_saved_on_200', async () => {
    const db = await openDb('owner-1');
    const env = new TestEnv();
    let resolveSend: ((val: unknown) => void) | null = null;
    const send = vi.fn().mockImplementation(() => {
      return new Promise((resolve) => {
        resolveSend = resolve;
      });
    });
    const journal = new OpJournal(db, send, env);

    const model = new RequestsModel('conv-1');
    model.items.set('req-1', {
      id: 'req-1',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: { command: 'rm -rf /tmp' }
    });

    const decision: DecisionBody = {
      kind: 'permission',
      behavior: 'allow'
    };

    const submitPromise = model.submitDecision('req-1', decision, journal);

    // Rule: No Fake Progress - state must be 'sending' locally, NOT 'saved'
    expect(model.items.get('req-1')!.state).toBe('sending');
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(model.items.get('req-1')!.state).toBe('sending');

    // Server responds 200 OK
    resolveSend!({
      request_id: 'req-1',
      winner_decision_id: 'dec-1',
      state: 'decision_saved',
      decision
    });

    await submitPromise;

    // After 200 OK, transitions to 'saved'
    expect(model.items.get('req-1')!.state).toBe('saved');
    expect(model.items.get('req-1')!.winnerDecision).toEqual(decision);
  });

  it('op_journal_retries_unknown_decision_with_same_idempotency_key', async () => {
    const db = await openDb('owner-1');
    const env = new TestEnv();
    const keysUsed: string[] = [];
    let attempts = 0;

    const send = vi.fn().mockImplementation((op: Op) => {
      keysUsed.push(op.key);
      attempts += 1;
      if (attempts === 1) {
        // First attempt fails transiently with NetworkError (retryable)
        throw new NetworkError('Connection failed');
      }
      return Promise.resolve({
        request_id: 'req-retry',
        winner_decision_id: 'dec-1',
        state: 'decision_saved',
        decision: op.body
      });
    });

    const journal = new OpJournal(db, send, env);

    const model = new RequestsModel('conv-1');
    model.items.set('req-retry', {
      id: 'req-retry',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: { command: 'git pull' }
    });

    const decision: DecisionBody = { kind: 'permission', behavior: 'allow' };
    const submitPromise = model.submitDecision('req-retry', decision, journal);

    await vi.waitFor(() => expect(keysUsed.length).toBe(1));
    const firstKey = keysUsed[0];

    // Advance timer for retry backoff (attempts 1 -> backoff is 1000ms)
    await env.advance(1100);
    await submitPromise;

    expect(keysUsed.length).toBe(2);
    expect(keysUsed[1]).toBe(firstKey); // Exactly same idempotency key reused!
    expect(model.items.get('req-retry')!.state).toBe('saved');
  });

  it('decision_conflict_409_updates_store_to_winner_and_disables_controls', async () => {
    const db = await openDb('owner-1');
    const env = new TestEnv();
    const winningDecision: DecisionBody = {
      kind: 'permission',
      behavior: 'deny',
      message: 'Blocked by another operator'
    };

    const send = vi.fn().mockImplementation(() => {
      throw new ApiError(
        409,
        'decision_conflict' as ErrorCode,
        'Conflict: already decided',
        'domain' as ErrorScope,
        'never',
        'c-conflict',
        {
          winner_decision: winningDecision,
          current_state: 'decision_saved'
        } as unknown as ErrorDetails
      );
    });

    const journal = new OpJournal(db, send, env);

    const model = new RequestsModel('conv-1');
    model.items.set('req-conflict', {
      id: 'req-conflict',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'open',
      payload: { command: 'reboot' }
    });

    const myDecision: DecisionBody = { kind: 'permission', behavior: 'allow' };

    // Should NOT throw an unhandled exception
    await expect(model.submitDecision('req-conflict', myDecision, journal)).resolves.toBeUndefined();

    const item = model.items.get('req-conflict')!;
    // State becomes 'saved' with the winner's decision
    expect(item.state).toBe('saved');
    expect(item.winnerDecision).toEqual(winningDecision);
  });

  it('decision_invalid_422_returns_the_card_to_open_with_an_error_and_allows_retry', async () => {
    const db = await openDb('owner-1');
    const env = new TestEnv();
    let refuse = true;
    const send = vi.fn().mockImplementation((op: Op) => {
      if (refuse) {
        throw new ApiError(422, 'decision_invalid' as ErrorCode, 'invalid answer', 'domain' as ErrorScope, 'never', 'c-422');
      }
      return Promise.resolve({ request_id: 'req-422', state: 'decision_saved', decision: (op.body as { decision: unknown }).decision });
    });
    const journal = new OpJournal(db, send, env);
    const model = new RequestsModel('conv-1');
    model.items.set('req-422', {
      id: 'req-422',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: { questions: [{ id: 'q1', text: 'Какой профиль?', options: [{ label: 'быстрый' }] }] }
    });
    const decision: DecisionBody = { kind: 'question', answers: { q1: '' } };

    await expect(model.submitDecision('req-422', decision, journal)).resolves.toBeUndefined();
    expect(model.items.get('req-422')!.state).toBe('open');
    expect(model.items.get('req-422')!.error).toBe('Ответ не подходит к вопросу');

    refuse = false;
    await model.submitDecision('req-422', { kind: 'question', answers: { q1: 'быстрый' } }, journal);
    expect(model.items.get('req-422')!.state).toBe('saved');
    expect(model.items.get('req-422')!.error).toBeUndefined();
  });

  it('journal_failure_before_sending_returns_the_card_to_open', async () => {
    const model = new RequestsModel('conv-1');
    model.items.set('req-idb', { id: 'req-idb', conversationId: 'conv-1', kind: 'permission', state: 'open', payload: {} });
    const journal = { begin: vi.fn(async () => { throw new Error('quota'); }) } as unknown as OpJournal;
    await expect(model.submitDecision('req-idb', { kind: 'permission', behavior: 'allow' }, journal)).resolves.toBeUndefined();
    expect(model.items.get('req-idb')!.state).toBe('open');
    expect(model.items.get('req-idb')!.error).toBe('Не удалось сохранить решение на устройстве');
  });

  it('decided_requests_leave_no_journal_row_and_secret_answers_are_never_written', async () => {
    const db = await openDb('owner-1');
    const written: string[] = [];
    const realPut = db.put.bind(db);
    db.put = async (store, key, value) => {
      if (store === 'ops') written.push(JSON.stringify(value));
      return realPut(store, key, value);
    };
    const env = new TestEnv();
    const send = vi.fn(async (op: Op) => ({ state: 'decision_saved', decision: (op.body as { decision: unknown }).decision }));
    const journal = new OpJournal(db, send, env);
    const model = new RequestsModel('conv-1');
    model.items.set('req-plain', { id: 'req-plain', conversationId: 'conv-1', kind: 'permission', state: 'open', payload: {} });
    model.items.set('req-secret', {
      id: 'req-secret',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: { questions: [{ id: 'q1', text: 'Токен', is_secret: true, allow_custom: true }] }
    });

    await model.submitDecision('req-plain', { kind: 'permission', behavior: 'allow' }, journal);
    await model.submitDecision('req-secret', { kind: 'question', answers: { q1: 'ghp_тайна' } }, journal);

    expect(model.items.get('req-plain')!.state).toBe('saved');
    expect(model.items.get('req-secret')!.state).toBe('saved');
    expect(send).toHaveBeenCalledTimes(2);
    expect(journal.ops.size).toBe(0);
    expect(await db.list('ops')).toEqual([]);
    expect(written.join('\n')).not.toContain('ghp_тайна');
  });

  it('request_closed_updates_state_to_applied', () => {
    const model = new RequestsModel('conv-1');
    model.items.set('req-close', {
      id: 'req-close',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'saved',
      payload: { questions: [] }
    });

    const appliedEvent = {
      v: 1,
      id: 'ev-close-1',
      scope: 'conversation:conv-1',
      scope_seq: '5',
      scope_seq_from: '5',
      committed_at: '2026-10-29T12:00:00Z',
      committed_at_from: '2026-10-29T12:00:00Z',
      conversation_id: 'conv-1',
      execution_id: 'exec-1',
      correlation_id: 'corr-1',
      kind: 'request_closed',
      payload: {
        request_id: 'req-close',
        reason: 'applied'
      }
    } as unknown as HubEvent;

    model.applyClosed(appliedEvent);

    expect(model.items.get('req-close')!.state).toBe('applied');
    expect(model.items.get('req-close')!.closedReason).toBe('applied');

    // Also test not_applied
    model.items.set('req-not-applied', {
      id: 'req-not-applied',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'saved',
      payload: {}
    });

    model.applyClosed({
      ...appliedEvent,
      payload: {
        request_id: 'req-not-applied',
        reason: 'not_applied'
      }
    } as unknown as HubEvent);

    expect(model.items.get('req-not-applied')!.state).toBe('not_applied');
    expect(model.items.get('req-not-applied')!.closedReason).toBe('not_applied');
  });

  it('decision_the_hub_did_not_apply_is_not_applied_under_any_reason', () => {
    const model = new RequestsModel('conv-1');
    const close = (id: string, reason: string) =>
      model.applyClosed({
        kind: 'request_closed',
        payload: { request_id: id, reason }
      } as unknown as HubEvent);
    const seed = (id: string, state: 'saved' | 'transmitting' | 'open') =>
      model.items.set(id, { id, conversationId: 'conv-1', kind: 'permission', state, payload: {} });

    seed('saved', 'saved');
    close('saved', 'unsupported_command');
    expect(model.items.get('saved')!.state).toBe('not_applied');
    expect(model.items.get('saved')!.closedReason).toBe('unsupported_command');

    seed('moving', 'transmitting');
    close('moving', 'execution_ended');
    expect(model.items.get('moving')!.state).toBe('not_applied');

    seed('open', 'open');
    close('open', 'execution_ended');
    expect(model.items.get('open')!.state).toBe('closed');
  });

  it('turn_interrupt_event_closes_pending_requests', () => {
    const model = new RequestsModel('conv-1');
    model.items.set('req-open', {
      id: 'req-open',
      conversationId: 'conv-1',
      kind: 'question',
      state: 'open',
      payload: {}
    });
    model.items.set('req-sending', {
      id: 'req-sending',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'sending',
      payload: {}
    });
    model.items.set('req-applied', {
      id: 'req-applied',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'applied',
      payload: {}
    });

    const interruptEvent = {
      v: 1,
      id: 'ev-int-1',
      scope: 'conversation:conv-1',
      scope_seq: '6',
      scope_seq_from: '6',
      committed_at: '2026-10-29T12:00:00Z',
      committed_at_from: '2026-10-29T12:00:00Z',
      conversation_id: 'conv-1',
      execution_id: 'exec-1',
      correlation_id: 'corr-1',
      kind: 'interrupt_state_changed',
      payload: {
        command_id: 'cmd-1',
        status: 'completed'
      }
    } as unknown as HubEvent;

    model.applyInterrupt(interruptEvent);

    expect(model.items.get('req-open')!.state).toBe('closed');
    expect(model.items.get('req-open')!.closedReason).toBe('interrupted');

    expect(model.items.get('req-sending')!.state).toBe('closed');
    expect(model.items.get('req-sending')!.closedReason).toBe('interrupted');

    // Applied request was already terminal, remains applied
    expect(model.items.get('req-applied')!.state).toBe('applied');
  });

  it('request_closed_with_unconfirmed_reason_sets_its_own_state', () => {
    const model = new RequestsModel('conv-1');
    model.items.set('req-unconfirmed', {
      id: 'req-unconfirmed',
      conversationId: 'conv-1',
      kind: 'permission',
      state: 'transmitting',
      payload: {}
    });

    model.applyClosed({
      kind: 'request_closed',
      payload: { request_id: 'req-unconfirmed', reason: 'unconfirmed' }
    } as unknown as HubEvent);

    expect(model.items.get('req-unconfirmed')!.state).toBe('unconfirmed');
    expect(model.items.get('req-unconfirmed')!.closedReason).toBe('unconfirmed');

    // Позднее доказательство вторым событием переводит в applied.
    model.applyClosed({
      kind: 'request_closed',
      payload: { request_id: 'req-unconfirmed', reason: 'applied' }
    } as unknown as HubEvent);

    expect(model.items.get('req-unconfirmed')!.state).toBe('applied');
  });

  it('snapshot_maps_decision_unconfirmed_to_its_own_state', () => {
    const model = new RequestsModel('conv-1');
    model.ingestSnapshot([
      {
        id: 'req-snap-unconfirmed',
        conversation_id: 'conv-1',
        kind: 'permission',
        state: 'decision_unconfirmed',
        request_payload: { tool_name: 'mcp_elicitation' }
      }
    ]);

    expect(model.items.get('req-snap-unconfirmed')!.state).toBe('unconfirmed');
  });
});
