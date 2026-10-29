import { getConfig } from "#lib/api/client.js";
import type { components } from "#lib/api/generated/openapi.js";
import { notifyIncompatible } from "#lib/pwa/update.js";

export type LimitsView = components["schemas"]["LimitsView"];

const DEFAULTS = {
  maxBytes: 104_857_600,
  maxPerInput: 10,
  sliceBytes: 8_388_608,
};

let limits: LimitsView | null = null;
export function resetConfig(): void {
  limits = null;
}

export async function loadConfig(): Promise<
  components["schemas"]["ConfigView"]
> {
  const response = await getConfig();
  limits = response.data.limits;
  if (response.data.schema_version !== 1) notifyIncompatible();
  return response.data;
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
