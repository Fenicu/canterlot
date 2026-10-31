import * as client from "#lib/api/client.js";
import type { components } from "#lib/api/generated/openapi.js";

export type SpendOutcome = components["schemas"]["SpendOutcome"];
export type SettingsPatch = components["schemas"]["SettingsPatch"];
export type CrunchSetting = components["schemas"]["CrunchSetting"];

export async function stopSession(
  conversationId: string,
  opts?: { force?: boolean; key?: string },
) {
  const key = opts?.key ?? crypto.randomUUID();
  return client.stop(conversationId, { force: opts?.force ?? false }, key);
}

export async function resumeSession(
  conversationId: string,
  opts?: { continue_queue?: boolean; key?: string },
) {
  const key = opts?.key ?? crypto.randomUUID();
  return client.resume(
    conversationId,
    { continue_queue: opts?.continue_queue ?? false },
    key,
  );
}

export async function continueSessionQueue(
  conversationId: string,
  key = crypto.randomUUID(),
) {
  return client.continueQueue(conversationId, key);
}

export async function spendSessionReserve(
  conversationId: string,
  key = crypto.randomUUID(),
): Promise<{ outcome: SpendOutcome; message?: string }> {
  const res = await client.spendReserve(conversationId, key);
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
  key = crypto.randomUUID(),
) {
  return client.setCrunch(conversationId, body, key);
}

export async function updateSettings(
  conversationId: string,
  body: SettingsPatch,
  key = crypto.randomUUID(),
) {
  return client.patchSettings(conversationId, body, key);
}
