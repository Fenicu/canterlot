import * as client from "#lib/api/client.js";
import type { components } from "#lib/api/generated/openapi.js";
import { intents } from "#lib/ops/intent.js";

export type SpendOutcome = components["schemas"]["SpendOutcome"];
export type SettingsPatch = components["schemas"]["SettingsPatch"];
export type CrunchSetting = components["schemas"]["CrunchSetting"];

/**
 * Команда без ключа от вызывающего идёт через намерение: двойное нажатие ждёт тот же запрос,
 * а повтор после обрыва связи уходит тем же ключом.
 */
function once<T>(
  scope: string,
  key: string | undefined,
  call: (key: string) => Promise<T>,
): Promise<T> {
  return key ? call(key) : intents.run(scope, call);
}

/** Команда ещё в полёте: кнопку стоит показать занятой. */
export function controlBusy(scope: string): boolean {
  return intents.busy(scope);
}

export async function stopSession(
  conversationId: string,
  opts?: { force?: boolean; key?: string },
) {
  const force = opts?.force ?? false;
  return once(`stop:${conversationId}:${force}`, opts?.key, (key) =>
    client.stop(conversationId, { force }, key),
  );
}

export async function resumeSession(
  conversationId: string,
  opts?: { continue_queue?: boolean; key?: string },
) {
  const continueQueue = opts?.continue_queue ?? false;
  return once(`resume:${conversationId}:${continueQueue}`, opts?.key, (key) =>
    client.resume(conversationId, { continue_queue: continueQueue }, key),
  );
}

export async function continueSessionQueue(
  conversationId: string,
  key?: string,
) {
  return once(`continue:${conversationId}`, key, (used) =>
    client.continueQueue(conversationId, used),
  );
}

export async function resolveOwnerLost(conversationId: string, key?: string) {
  return once(`resolve:${conversationId}`, key, (used) =>
    client.resolveOwnerLost(conversationId, used),
  );
}

export async function compactSessionContext(
  conversationId: string,
  key?: string,
) {
  return once(`compact:${conversationId}`, key, (used) =>
    client.compact(conversationId, used),
  );
}

export async function spendSessionReserve(
  conversationId: string,
  key?: string,
): Promise<{ outcome: SpendOutcome; message?: string }> {
  const res = await once(`spend:${conversationId}`, key, (used) =>
    client.spendReserve(conversationId, used),
  );
  const outcome = res.data;
  let message: string | undefined;
  if (outcome === "already_continued") {
    message = "Резерв уже потрачен в этой сессии";
  } else if (outcome === "not_held") {
    message = "Пауза уже снята";
  }
  return { outcome, message };
}

export async function updateCrunch(
  conversationId: string,
  body: CrunchSetting,
  key?: string,
) {
  return once(`crunch:${conversationId}:${JSON.stringify(body)}`, key, (used) =>
    client.setCrunch(conversationId, body, used),
  );
}

export async function updateSettings(
  conversationId: string,
  body: SettingsPatch,
  key?: string,
) {
  return once(
    `settings:${conversationId}:${JSON.stringify(body)}`,
    key,
    (used) => client.patchSettings(conversationId, body, used),
  );
}

export async function acceptSession(
  conversationId: string,
  opts?: { force?: boolean; key?: string },
) {
  const force = opts?.force ?? false;
  return once(`accept:${conversationId}:${force}`, opts?.key, (key) =>
    client.accept(conversationId, { force }, key),
  );
}
