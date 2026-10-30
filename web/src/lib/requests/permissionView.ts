export interface PermissionField {
  label: string;
  value: string;
}

export interface PermissionChange {
  path: string;
  kind: string;
  diff: string;
}

export interface PermissionView {
  command: string;
  description: string;
  reason: string;
  facts: PermissionField[];
  changes: PermissionChange[];
  fields: PermissionField[];
  denyInterrupts: boolean;
}

const CHANGE_KIND: Record<string, string> = {
  add: "Новый файл",
  delete: "Удаление",
  update: "Изменение",
};

const SHOWN_INPUT = new Set(["command", "description", "file_path"]);

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string {
  if (value === null || value === undefined) return "";
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

function change(raw: unknown): PermissionChange {
  const item = record(raw);
  const kind = text(record(item.kind).type ?? item.kind);
  return {
    path: text(item.path),
    kind: CHANGE_KIND[kind] ?? kind,
    diff: text(item.diff),
  };
}

/** Адаптеры кладут суть запроса в разные поля: Claude Code в `input`, Codex на верхний уровень. */
export function permissionView(payload: unknown): PermissionView {
  const top = record(payload);
  const input = record(top.input);
  const facts = [
    { label: "Файл", value: text(input.file_path) },
    { label: "Папка", value: text(top.cwd) },
    { label: "Доступ к папке", value: text(top.grant_root) },
    { label: "Сеть", value: text(top.network) },
  ].filter((fact) => fact.value);
  // Codex повторяет в `input` тот же запрос, что и на верхнем уровне.
  const rest =
    top.command === undefined || top.command === null
      ? Object.entries(input).filter(([key]) => !SHOWN_INPUT.has(key))
      : [];
  const fields = rest.map(([label, value]) => ({ label, value: text(value) }));
  if (top.permissions !== undefined && top.permissions !== null)
    fields.unshift({ label: "Права", value: text(top.permissions) });
  return {
    command: text(top.command ?? input.command),
    description: text(top.description ?? input.description),
    reason: text(top.reason),
    facts,
    changes: Array.isArray(top.changes) ? top.changes.map(change) : [],
    fields,
    denyInterrupts: top.deny_also_interrupts === true,
  };
}
