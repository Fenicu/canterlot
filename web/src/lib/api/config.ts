import { getConfig } from "#lib/api/client.js";
import type { components } from "#lib/api/generated/openapi.js";
import { notifyIncompatible } from "#lib/pwa/update.js";
import { setLimitThresholds } from "#lib/stores/limitThresholds.svelte.js";

export type LimitsView = components["schemas"]["LimitsView"];
export type ConfigView = components["schemas"]["ConfigView"];

const DEFAULTS = {
  maxBytes: 104_857_600,
  maxPerInput: 10,
  sliceBytes: 8_388_608,
};

let limits: LimitsView | null = null;
let config: ConfigView | null = null;

export function resetConfig(): void {
  limits = null;
  config = null;
}

/** Последний загруженный конфиг: экраны читают его без повторного запроса. */
export function cachedConfig(): ConfigView | null {
  return config;
}

export async function loadConfig(): Promise<ConfigView> {
  const response = await getConfig();
  limits = response.data.limits;
  config = response.data;
  setLimitThresholds(response.data.limit_thresholds);
  if (response.data.schema_version !== 1) notifyIncompatible();
  return response.data;
}

/** После смены порогов владельцем: кэш обновляется, чтобы другие экраны увидели новые значения. */
export async function refreshConfig(): Promise<ConfigView> {
  return loadConfig();
}

export function uploadLimits(): {
  maxBytes: number;
  maxPerInput: number;
  sliceBytes: number;
} {
  return {
    maxBytes: limits?.max_attachment_bytes ?? DEFAULTS.maxBytes,
    maxPerInput: limits?.max_attachments_per_input ?? DEFAULTS.maxPerInput,
    sliceBytes: limits?.upload_chunk_bytes ?? DEFAULTS.sliceBytes,
  };
}
