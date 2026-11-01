// Логика редактора белого списка делегирования: записи, каталог машин, вызовы API.
// Маршрутов ещё нет в сгенерированной схеме, поэтому типы и вызовы описаны здесь.
import { http, type RequestOptions } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";

export interface WhitelistEntry {
  harness: string;
  model: string;
}

export interface WhitelistView {
  entries: WhitelistEntry[];
  is_default: boolean;
  default_entries: WhitelistEntry[];
}

export type CatalogHarness = Pick<components["schemas"]["HarnessView"], "id" | "title"> & {
  models: Pick<components["schemas"]["ModelView"], "id" | "title">[];
};

export interface EntryRow extends WhitelistEntry {
  harnessTitle: string;
  modelTitle: string;
  known: boolean;
}

export interface PickerOption {
  value: string;
  label: string;
}

export const MAX_ENTRIES = 64;
export const DEFAULT_MODEL = "default";
export const DEFAULT_MODEL_LABEL = "Модель по умолчанию";
export const NOT_IN_CATALOG = "Нет в каталоге машины";

const same = (a: WhitelistEntry, b: WhitelistEntry): boolean => a.harness === b.harness && a.model === b.model;

export function getSpawnWhitelist(opts?: RequestOptions) {
  return http<WhitelistView>("GET", "/api/v1/spawn-whitelist", undefined, opts);
}

// entries === null сбрасывает список к встроенному.
export function putSpawnWhitelist(entries: WhitelistEntry[] | null, key: string, opts?: RequestOptions) {
  return http<WhitelistView>("PUT", "/api/v1/spawn-whitelist", { entries }, { ...opts, idempotencyKey: key });
}

// Каталоги нескольких машин сливаются: харнесс и модель берутся один раз.
export function mergeCatalog(machines: { harnesses: CatalogHarness[] }[]): CatalogHarness[] {
  const out = new Map<string, { id: string; title: string; models: Map<string, { id: string; title: string }> }>();
  for (const machine of machines) {
    for (const harness of machine.harnesses) {
      const slot = out.get(harness.id) ?? { id: harness.id, title: harness.title, models: new Map() };
      for (const model of harness.models) if (!slot.models.has(model.id)) slot.models.set(model.id, { id: model.id, title: model.title });
      out.set(harness.id, slot);
    }
  }
  return [...out.values()].map((slot) => ({ id: slot.id, title: slot.title, models: [...slot.models.values()] }));
}

export function describeEntries(entries: WhitelistEntry[], catalog: CatalogHarness[]): EntryRow[] {
  return entries.map((entry) => {
    const harness = catalog.find((item) => item.id === entry.harness);
    const model = entry.model === DEFAULT_MODEL ? undefined : harness?.models.find((item) => item.id === entry.model);
    const isDefault = entry.model === DEFAULT_MODEL;
    return {
      ...entry,
      harnessTitle: harness?.title ?? entry.harness,
      modelTitle: isDefault ? DEFAULT_MODEL_LABEL : (model?.title ?? entry.model),
      known: harness !== undefined && (isDefault || model !== undefined),
    };
  });
}

export function harnessOptions(catalog: CatalogHarness[]): PickerOption[] {
  return catalog.map((harness) => ({ value: harness.id, label: harness.title }));
}

export function modelOptions(catalog: CatalogHarness[], harnessId: string): PickerOption[] {
  const models = catalog.find((item) => item.id === harnessId)?.models ?? [];
  return [{ value: DEFAULT_MODEL, label: DEFAULT_MODEL_LABEL }, ...models.map((model) => ({ value: model.id, label: model.title }))];
}

export type AddResult = { ok: true; entries: WhitelistEntry[] } | { ok: false; error: string };

export function addEntry(entries: WhitelistEntry[], entry: WhitelistEntry): AddResult {
  if (entries.some((item) => same(item, entry))) return { ok: false, error: "Такая запись уже есть" };
  if (entries.length >= MAX_ENTRIES) return { ok: false, error: `Не больше ${MAX_ENTRIES} записей` };
  return { ok: true, entries: [...entries, entry] };
}

export function removeEntry(entries: WhitelistEntry[], entry: WhitelistEntry): WhitelistEntry[] {
  return entries.filter((item) => !same(item, entry));
}

export function sameEntries(a: WhitelistEntry[], b: WhitelistEntry[]): boolean {
  return a.length === b.length && a.every((item, n) => same(item, b[n]!));
}
