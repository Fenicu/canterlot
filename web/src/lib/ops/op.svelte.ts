import type { Op } from "#lib/ops/journal.js";

export function reactiveOp<B>(init: Op<B>): Op<B> {
  let state = $state(init.state);
  let attempts = $state(init.attempts);
  let refusal = $state(init.refusal);
  let result = $state(init.result);

  return {
    key: init.key,
    kind: init.kind,
    conversationId: init.conversationId,
    body: init.body,
    createdAt: init.createdAt,
    get state() {
      return state;
    },
    set state(value) {
      state = value;
    },
    get attempts() {
      return attempts;
    },
    set attempts(value) {
      attempts = value;
    },
    get refusal() {
      return refusal;
    },
    set refusal(value) {
      refusal = value;
    },
    get result() {
      return result;
    },
    set result(value) {
      result = value;
    },
  };
}
