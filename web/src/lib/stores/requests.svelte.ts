import { SvelteMap } from 'svelte/reactivity';
import type { HubEvent } from '#lib/sync/wire.js';
import type { Op, OpJournal } from '#lib/ops/journal.js';
import type { DecisionBody } from '#lib/api/client.js';
import { ApiError } from '#lib/api/http.js';
import { describeError } from '#lib/api/errors.js';

export type RequestClientState =
  | 'open'
  | 'sending'
  | 'saved'
  | 'transmitting'
  | 'unconfirmed'
  | 'applied'
  | 'not_applied'
  | 'closed';

export type RequestKind = 'permission' | 'question' | 'spawn_approval';

/** Неизвестный вид запроса остаётся вопросом, а карточка одобрения запуска имеет свой вид. */
export function requestKind(raw: unknown): RequestKind {
  return raw === 'permission' || raw === 'spawn_approval' ? raw : 'question';
}

export interface RequestItem {
  id: string;
  conversationId: string;
  kind: RequestKind;
  state: RequestClientState;
  payload: unknown;
  winnerDecision?: unknown;
  closedReason?: string;
  error?: string;
}

function asRecord(val: unknown): Record<string, unknown> {
  return val && typeof val === 'object' ? (val as Record<string, unknown>) : {};
}

export function createRequestItem(init: RequestItem): RequestItem {
  const item = $state<RequestItem>({
    id: init.id,
    conversationId: init.conversationId,
    kind: init.kind,
    state: init.state,
    payload: init.payload,
    winnerDecision: init.winnerDecision,
    closedReason: init.closedReason,
    error: init.error
  });
  return item;
}

/** Открытый запрос стоит у поля ввода, чтобы его не пропустили; решённый остаётся в ленте на своём месте. */
export function isDocked(request: Pick<RequestItem, 'state'>): boolean {
  return request.state === 'open' || request.state === 'sending';
}

/** Секретный вопрос: ответ живёт только в памяти, на устройство не пишется. */
export function hasSecretQuestion(payload: unknown): boolean {
  const record = asRecord(payload);
  const nested = asRecord(record.payload);
  const list = Array.isArray(record.questions)
    ? record.questions
    : Array.isArray(nested.questions)
      ? nested.questions
      : [];
  return (
    Boolean(record.is_secret ?? record.isSecret) ||
    list.some((q) => {
      const question = asRecord(q);
      return Boolean(question.is_secret ?? question.isSecret);
    })
  );
}

export class RequestsModel {
  readonly items = new SvelteMap<string, RequestItem>();
  readonly conversationId?: string;

  constructor(conversationId?: string) {
    this.conversationId = conversationId;
  }

  ingestSnapshot(requests: unknown[]): void {
    if (!Array.isArray(requests)) return;
    for (const raw of requests) {
      if (!raw || typeof raw !== 'object') continue;
      const r = raw as Record<string, unknown>;
      const id = String(r.id ?? r.request_id ?? '');
      if (!id) continue;
      const conversationId = String(r.conversation_id ?? r.conversationId ?? this.conversationId ?? '');
      const kind = requestKind(r.kind);
      const payload = r.request_payload ?? r.payload ?? {};
      const rawState = String(r.state ?? 'open');
      let state: RequestClientState = 'open';
      if (rawState === 'decision_saved' || rawState === 'saved') {
        state = 'saved';
      } else if (rawState === 'decision_transmitting' || rawState === 'transmitting') {
        state = 'transmitting';
      } else if (rawState === 'decision_unconfirmed' || rawState === 'unconfirmed') {
        state = 'unconfirmed';
      } else if (rawState === 'decision_applied' || rawState === 'applied') {
        state = 'applied';
      } else if (rawState === 'decision_not_applied' || rawState === 'not_applied') {
        state = 'not_applied';
      } else if (rawState === 'closed_without_decision' || rawState === 'closed') {
        state = 'closed';
      } else if (rawState === 'sending') {
        state = 'sending';
      }
      const winnerDecision = r.decision_payload ?? r.winner_decision ?? r.winnerDecision;
      const closedReason = typeof r.reason === 'string' ? r.reason : (typeof r.closed_reason === 'string' ? r.closed_reason : undefined);

      const existing = this.items.get(id);
      if (existing) {
        if (existing.state === 'sending' && state === 'open') {
          continue;
        }
        existing.conversationId = conversationId;
        existing.kind = kind;
        existing.state = state;
        existing.payload = payload;
        if (winnerDecision !== undefined) existing.winnerDecision = winnerDecision;
        if (closedReason !== undefined) existing.closedReason = closedReason;
      } else {
        this.items.set(id, createRequestItem({
          id,
          conversationId,
          kind,
          state,
          payload,
          winnerDecision,
          closedReason
        }));
      }
    }
  }

  /** Строка истории: та же запись запроса, что и в снимке, только с местом в ленте. */
  ingestHistory(row: { id: string; request_kind: string; state: string; request_payload: unknown; decision_payload?: unknown }): RequestItem {
    this.ingestSnapshot([{ ...row, kind: row.request_kind }]);
    return this.items.get(row.id)!;
  }

  applyOpened(event: HubEvent): void {
    const ev = asRecord(event);
    const payload = asRecord(event.payload);
    const id = String(payload.request_id ?? payload.id ?? '');
    if (!id) return;
    const conversationId = String(ev.conversation_id ?? payload.conversation_id ?? this.conversationId ?? '');
    const kind = requestKind(payload.kind);
    const requestPayload = payload.payload ?? payload;

    const existing = this.items.get(id);
    if (existing) {
      existing.conversationId = conversationId;
      existing.kind = kind;
      existing.payload = requestPayload;
      if (existing.state !== 'sending' && existing.state !== 'saved') {
        existing.state = 'open';
      }
    } else {
      this.items.set(id, createRequestItem({
        id,
        conversationId,
        kind,
        state: 'open',
        payload: requestPayload
      }));
    }
  }

  applyDecided(event: HubEvent): void {
    const ev = asRecord(event);
    const payload = asRecord(event.payload);
    const id = String(payload.request_id ?? payload.id ?? '');
    if (!id) return;
    const decision = payload.decision ?? payload.winner_decision;
    const item = this.items.get(id);
    if (item) {
      item.state = 'saved';
      item.winnerDecision = decision;
    } else {
      const conversationId = String(ev.conversation_id ?? payload.conversation_id ?? this.conversationId ?? '');
      this.items.set(id, createRequestItem({
        id,
        conversationId,
        kind: 'question',
        state: 'saved',
        payload: {},
        winnerDecision: decision
      }));
    }
  }

  applyTransmitting(event: HubEvent): void {
    const payload = asRecord(event.payload);
    const id = String(payload.request_id ?? payload.id ?? '');
    if (!id) return;
    const item = this.items.get(id);
    if (item) {
      if (item.state === 'saved' || item.state === 'sending' || item.state === 'open') {
        item.state = 'transmitting';
      }
    }
  }

  applyClosed(event: HubEvent): void {
    const payload = asRecord(event.payload);
    const id = String(payload.request_id ?? payload.id ?? '');
    if (!id) return;
    const item = this.items.get(id);
    const reason = typeof payload.reason === 'string' ? payload.reason : undefined;
    if (item) {
      // У решения, не дошедшего до агента, хаб передаёт причину работника, а не `not_applied`.
      const decided = item.state === 'saved' || item.state === 'transmitting';
      item.closedReason = reason;
      if (reason === 'applied') {
        item.state = 'applied';
      } else if (reason === 'unconfirmed') {
        item.state = 'unconfirmed';
      } else if (reason === 'not_applied' || decided) {
        item.state = 'not_applied';
      } else {
        item.state = 'closed';
      }
    }
  }

  applyInterrupt(event?: HubEvent): void {
    void event;
    for (const item of this.items.values()) {
      if (item.state === 'open' || item.state === 'sending') {
        item.state = 'closed';
        item.closedReason = 'interrupted';
      }
    }
  }

  async submitDecision(
    requestId: string,
    decision: DecisionBody,
    journal: OpJournal
  ): Promise<void> {
    const item = this.items.get(requestId);
    const conversationId = item?.conversationId ?? this.conversationId ?? '';

    // No Fake Progress: card state transitions to 'sending' locally upon clicking submit.
    // It ONLY transitions to 'saved' when the hub commits (200 OK or request_decided).
    if (item) {
      item.state = 'sending';
      item.error = undefined;
    }

    let op: Op;
    try {
      op = await journal.begin(
        'other',
        { requestId, conversationId, decision },
        { conversationId, persist: !hasSecretQuestion(item?.payload) }
      );
    } catch {
      if (item && item.state === 'sending') {
        item.state = 'open';
        item.error = 'Не удалось сохранить решение на устройстве';
      }
      return;
    }

    try {
      await journal.run(op);
      if (item && item.state === 'sending' && op.state === 'committed') {
        item.state = 'saved';
        if (op.result && typeof op.result === 'object') {
          const res = op.result as Record<string, unknown>;
          if ('decision' in res) {
            item.winnerDecision = res.decision;
          } else if ('winner_decision' in res) {
            item.winnerDecision = res.winner_decision;
          }
        }
      }
    } catch (err) {
      if (err instanceof ApiError) {
        if (err.code === 'decision_conflict') {
          if (item) {
            item.state = 'saved';
            item.error = 'decision_conflict';
            const details = err.details as Record<string, unknown> | undefined;
            if (details?.winner_decision) {
              item.winnerDecision = details.winner_decision;
            }
          }
          return;
        }
        if (err.code === 'request_not_open') {
          if (item) {
            item.state = 'closed';
            const details = err.details as Record<string, unknown> | undefined;
            if (typeof details?.reason === 'string') {
              item.closedReason = details.reason;
            } else {
              item.closedReason = 'closed';
            }
          }
          return;
        }
      }
      // Отказ хаба или сбой клиента: карточка снова доступна, причина видна, ответ можно исправить.
      if (item && item.state === 'sending') {
        item.state = 'open';
        item.error = describeError(err).message;
      }
    } finally {
      // Решение доставлено или отклонено: строка журнала с ответом больше не нужна.
      journal.settle(op.key);
    }
  }
}
