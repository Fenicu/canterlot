import { recentWorkdirs } from "#lib/api/client.js";
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
  profile_unknown: "Профиль инструкций неизвестен",
  workdir_not_allowed: "Рабочая папка не разрешена",
  group_not_found: "Группа не найдена",
};

export function workdirProblem(path: string, roots: string[]): string | null {
  if (!path.startsWith("/")) return "Укажите абсолютный путь";
  const parts = path.split("/");
  if (parts.includes("..")) return "Путь не должен содержать ..";
  const clean = parts.filter((part) => part.length > 0);
  const under = roots.some((root) => {
    const rootParts = root.split("/").filter((part) => part.length > 0);
    if (clean.length < rootParts.length) return false;
    return rootParts.every((part, index) => part === clean[index]);
  });
  if (!under) return "Папка вне известных корней машины";
  return null;
}

export function machineProblem(
  machine: MachineRecord | undefined,
): string | null {
  if (!machine) return "Выберите машину";
  if (
    machine.availability === "unavailable" ||
    machine.availability === "degraded"
  ) {
    return `Машина недоступна: ${machine.availability}`;
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
  profile = "";
  crunch = false;
  resumeText: string;
  recent: string[] = [];
  #server: Partial<Record<Field, string>> = {};
  #key = "";

  constructor(
    private machines: MachinesStore,
    readonly config: Config,
    private ops: OpJournal,
  ) {
    this.resumeText = config.defaults?.crunch_resume_text ?? "";
  }

  get errors(): Partial<Record<Field, string>> {
    const machine = this.machines.items.get(this.machineId);
    const out: Partial<Record<Field, string>> = { ...this.#server };
    const machineError = this.machineId ? machineProblem(machine) : null;
    if (machineError) out.machine = machineError;
    if (this.workdir) {
      const work = workdirProblem(this.workdir, machine?.allowed_roots ?? []);
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
    if (!this.machineId) {
      this.recent = [];
      return;
    }
    const response = await recentWorkdirs(this.machineId);
    this.recent = response.data.workdirs.slice(0, 20);
  }

  async submit(): Promise<{ conversationId: string }> {
    const gate = this.canSubmit;
    if (!gate.ok) throw new Error(gate.reason);
    const op = await this.ops.begin(
      "create_conversation",
      {
        machine_id: this.machineId,
        workdir: this.workdir,
        harness: this.harness,
        model: this.model,
        effort: this.effort,
        instruction_profile: this.profile || null,
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
        if (field)
          this.#server = {
            ...this.#server,
            [field]: MESSAGE[err.code] ?? err.message,
          };
      }
      throw err;
    }
    const result = op.result as { conversation_id?: string } | undefined;
    const conversationId = result?.conversation_id ?? "";
    if (!conversationId) throw new Error("empty_conversation");
    return { conversationId };
  }
}
