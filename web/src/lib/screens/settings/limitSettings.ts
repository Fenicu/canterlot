import type { components } from "#lib/api/generated/openapi.js";

export type LimitSettings = components["schemas"]["LimitSettings"];
export type LimitSettingsView = components["schemas"]["LimitSettingsView"];

/** Поля формы держат `null`, пока ввод пуст: так видно, что именно не заполнено. */
export interface LimitSettingsDraft {
  warn: number | null;
  danger: number | null;
  reserve: number | null;
}

export interface LimitSettingsErrors {
  warn?: string;
  danger?: string;
  reserve?: string;
  relation?: string;
}

export type LimitSettingsCheck =
  | { ok: true; settings: LimitSettings }
  | { ok: false; errors: LimitSettingsErrors };

const PERCENT_ERROR = "Введите целое число от 0 до 99";
const RELATION_ERROR = "Красный порог не больше жёлтого";

function percent(value: number | null): value is number {
  return value !== null && Number.isInteger(value) && value >= 0 && value <= 99;
}

export function draftFrom(settings: LimitSettings): LimitSettingsDraft {
  return {
    warn: settings.warn_remaining_pct,
    danger: settings.danger_remaining_pct,
    reserve: settings.reserve_remaining_pct,
  };
}

export function checkDraft(draft: LimitSettingsDraft): LimitSettingsCheck {
  const errors: LimitSettingsErrors = {};
  if (!percent(draft.warn)) errors.warn = PERCENT_ERROR;
  if (!percent(draft.danger)) errors.danger = PERCENT_ERROR;
  if (!percent(draft.reserve)) errors.reserve = PERCENT_ERROR;
  if (errors.warn === undefined && errors.danger === undefined && draft.danger! > draft.warn!) {
    errors.relation = RELATION_ERROR;
  }
  if (Object.keys(errors).length > 0) return { ok: false, errors };
  return {
    ok: true,
    settings: {
      warn_remaining_pct: draft.warn!,
      danger_remaining_pct: draft.danger!,
      reserve_remaining_pct: draft.reserve!,
    },
  };
}

export function sameSettings(draft: LimitSettingsDraft, settings: LimitSettings): boolean {
  return draft.warn === settings.warn_remaining_pct
    && draft.danger === settings.danger_remaining_pct
    && draft.reserve === settings.reserve_remaining_pct;
}
