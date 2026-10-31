import { recentWorkdirs } from "#lib/api/client.js";
import { DEFAULT_PERMISSION_MODE, availabilityLabel } from "#lib/format/labels.js";
import { ApiError } from "#lib/api/http.js";
import type { components } from "#lib/api/generated/openapi.js";
import type { OpJournal } from "#lib/ops/journal.js";
import type {
  MachineRecord,
  MachinesStore,
} from "#lib/stores/machines.svelte.js";

export type SendGate = { ok: true } | { ok: false; reason: string };
export type Field =
  | "machine"
  | "workdir"
  | "harness"
  | "model"
  | "effort"
  | "profile"
  | "permission"
  | "group";

type Config = components["schemas"]["ConfigView"];

const FIELD_OF: Record<string, Field> = {
  machine_not_found: "machine",
  capability_missing: "machine",
  harness_unknown: "harness",
  model_unknown: "model",
  effort_not_allowed: "effort",
  permission_mode_unknown: "permission",
  profile_unknown: "profile",
  workdir_not_allowed: "workdir",
  group_not_found: "group",
};

const MESSAGE: Record<string, string> = {
  machine_not_found: "Машина не найдена",
  capability_missing: "У машины нет возможности пилота",
  harness_unknown: "Агент неизвестен",
  model_unknown: "Модель неизвестна",
  effort_not_allowed: "Усилие недоступно",
  permission_mode_unknown: "Режим разрешений неизвестен",
  profile_unknown: "Инструкции не найдены",
  workdir_not_allowed: "Рабочая папка не разрешена",
  group_not_found: "Группа не найдена",
};

export function workdirProblem(path: string): string | null {
  if (!path.startsWith("/")) return "Укажите абсолютный путь";
  const parts = path.split("/");
  if (parts.includes("..")) return "Путь не должен содержать ..";
  // Корни проверяет хаб: клиент принимает любой абсолютный путь.
  return null;
}

export function soleMachine(machines: { id: string }[]): string | null {
  return machines.length === 1 ? (machines[0]?.id ?? null) : null;
}

/** Путь, введённый владельцем, остаётся; подставленный ранее следует за машиной. */
export function prefillWorkdir(
  current: string,
  prefilled: string,
  home: string,
): string {
  return current === "" || current === prefilled ? home : current;
}

/** Каталог перечисляет модели от выбора по умолчанию, как и у Codex. */
export function defaultModelId(
  models: { id: string }[],
  current: string,
): string {
  return models.some((item) => item.id === current)
    ? current
    : (models[0]?.id ?? "");
}

/** У модели без вариантов усилие одно: выбирать нечего. Иначе выбор остаётся за владельцем. */
export function soleEffort(efforts: string[], current: string): string {
  return efforts.length === 1 ? (efforts[0] ?? current) : current;
}

function submittedWorkdir(path: string): string {
  return path.trim().replace(/(?<=.)\/+$/, "");
}

export function machineProblem(
  machine: MachineRecord | undefined,
): string | null {
  if (!machine) return "Выберите машину";
  if (
    machine.availability === "unavailable" ||
    machine.availability === "degraded"
  ) {
    return `Машина недоступна: ${availabilityLabel(machine.availability).toLocaleLowerCase("ru-RU")}`;
  }
  if (!(machine.capabilities ?? []).includes("pilot_v1"))
    return "Машина не готова к пилоту";
  return null;
}

export class NewSessionModel {
  machineId = "";
  workdir = "";
  harness = "";
  model = "";
  effort = "";
  permission = "";
  profile = "clean";
  crunch = false;
  resumeText: string;
  recent: string[] = [];
  #server: Partial<Record<Field, string>> = {};
  #refusedForm = "";
  #key = "";

  constructor(
    private machines: MachinesStore,
    readonly config: Config,
    private ops: OpJournal,
  ) {
    this.resumeText = config.defaults?.crunch_resume_text ?? "";
  }

  /** Отказ хаба относится ко всей форме: `capability_missing` снимает и смена пресета. */
  get #form(): string {
    return JSON.stringify([
      this.machineId,
      this.workdir,
      this.harness,
      this.model,
      this.effort,
      this.permission,
      this.profile,
    ]);
  }

  get errors(): Partial<Record<Field, string>> {
    const machine = this.machines.items.get(this.machineId);
    const out: Partial<Record<Field, string>> =
      this.#form === this.#refusedForm ? { ...this.#server } : {};
    const machineError = this.machineId ? machineProblem(machine) : null;
    if (machineError) out.machine = machineError;
    if (this.workdir) {
      const work = workdirProblem(this.workdir.trim());
      if (work) out.workdir = work;
    }
    return out;
  }

  get efforts(): string[] {
    const machine = this.machines.items.get(this.machineId);
    const harness = machine?.harnesses.find((item) => item.id === this.harness);
    return (
      harness?.models.find((item) => item.id === this.model)?.efforts ?? []
    );
  }

  get permissions(): string[] {
    const machine = this.machines.items.get(this.machineId);
    return (
      machine?.harnesses.find((item) => item.id === this.harness)
        ?.permission_modes ?? []
    );
  }

  /** Выбор владельца, а пока его нет, режим по умолчанию из каталога машины. */
  get permissionMode(): string {
    const modes = this.permissions;
    if (modes.includes(this.permission)) return this.permission;
    return modes.includes(DEFAULT_PERMISSION_MODE)
      ? DEFAULT_PERMISSION_MODE
      : (modes[0] ?? "");
  }

  get canSubmit(): SendGate {
    const errors = this.errors;
    if (!this.machineId) return { ok: false, reason: "Выберите машину" };
    if (errors.machine) return { ok: false, reason: errors.machine };
    if (!this.workdir) return { ok: false, reason: "Укажите рабочую папку" };
    if (errors.workdir) return { ok: false, reason: errors.workdir };
    if (!this.harness) return { ok: false, reason: "Выберите агента" };
    if (!this.model) return { ok: false, reason: "Выберите модель" };
    if (!this.effort || !this.efforts.includes(this.effort))
      return { ok: false, reason: "Выберите усилие" };
    return { ok: true };
  }

  async refreshRecent(): Promise<void> {
    const machineId = this.machineId;
    if (!machineId) {
      this.recent = [];
      return;
    }
    const response = await recentWorkdirs(machineId).catch(() => null);
    if (this.machineId !== machineId) return;
    this.recent = response?.data.workdirs.slice(0, 3) ?? [];
  }

  async submit(): Promise<{ conversationId: string }> {
    const gate = this.canSubmit;
    if (!gate.ok) throw new Error(gate.reason);
    const op = await this.ops.begin(
      "create_conversation",
      {
        machine_id: this.machineId,
        // Пробел с клавиатуры телефона дал бы сессию с несуществующей папкой, а косая
        // черта после выбранной подсказки завела бы вторую запись в недавних папках.
        workdir: submittedWorkdir(this.workdir),
        harness: this.harness,
        model: this.model,
        effort: this.effort,
        ...(this.permissionMode ? { permission_mode: this.permissionMode } : {}),
        instruction_profile: this.profile || "clean",
        crunch: {
          enabled: this.crunch,
          resume_text: this.crunch ? this.resumeText : null,
        },
        start: true,
      },
      { persist: true, key: this.#key || undefined },
    );
    this.#key = op.key;
    try {
      await this.ops.run(op);
    } catch (err) {
      if (err instanceof ApiError) {
        const field = FIELD_OF[err.code];
        if (field) {
          this.#server = { [field]: MESSAGE[err.code] ?? err.message };
          this.#refusedForm = this.#form;
        }
      }
      throw err;
    }
    const result = op.result as { conversation_id?: string } | undefined;
    const conversationId = result?.conversation_id ?? "";
    if (!conversationId) throw new Error("empty_conversation");
    return { conversationId };
  }
}
