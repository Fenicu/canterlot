import { describe, it, expect, vi } from "vitest";
import { InterruptControl, type InterruptDeps } from "#lib/composer/interrupt.svelte.js";

describe("InterruptControl", () => {
  it("interrupt_label_stays_until_terminal_status", async () => {
    const deps: InterruptDeps = { conversationId: "conv-1" };
    let calls = 0;
    deps.api = async () => {
      calls += 1;
    };
    const control = new InterruptControl(deps);
    const first = control.interrupt();
    const second = control.interrupt();
    await first;
    await second;
    expect(calls).toBe(1);
    expect(control.label).toBe("Прерываю…");
    expect(control.running).toBe(true);
    control.applyStatus("acknowledged");
    expect(control.label).toBe("Прерываю…");
    control.applyStatus("completed");
    expect(control.label).toBe("Прервано");
    expect(control.running).toBe(false);
    control.applyStatus("not_applicable");
    expect(control.label).toBe("Прерывание не требуется");
    control.applyStatus("superseded");
    expect(control.label).toBe("Прерывание заменено");
    control.applyStatus("rejected_stale");
    expect(control.label).toBe("Прерывание устарело");
  });
  it("interrupt_without_a_live_execution_finishes_at_once", async () => {
    const deps: InterruptDeps = { conversationId: "conv-1" };
    deps.api = async () => ({ requested: { interrupt_command_id: null, withdraw_command_id: null } });
    const control = new InterruptControl(deps);
    await control.interrupt();
    expect(control.label).toBe("Прерывание не требуется");
    expect(control.running).toBe(false);
  });

  it("interrupt_without_an_answer_from_the_machine_times_out_to_a_clear_state", async () => {
    vi.useFakeTimers();
    try {
      const deps: InterruptDeps = { conversationId: "conv-1" };
      deps.api = async () => ({ requested: { interrupt_command_id: "c1", withdraw_command_id: null } });
      const control = new InterruptControl(deps);
      await control.interrupt();
      expect(control.label).toBe("Прерываю…");
      await vi.advanceTimersByTimeAsync(20_000);
      expect(control.label).toBe("Прерывание не подтверждено");
      expect(control.running).toBe(false);
      let calls = 0;
      deps.api = async () => { calls += 1; };
      await control.interrupt();
      expect(calls).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("terminal_status_before_the_deadline_keeps_its_label", async () => {
    vi.useFakeTimers();
    try {
      const deps: InterruptDeps = { conversationId: "conv-1" };
      deps.api = async () => ({ requested: { interrupt_command_id: "c1", withdraw_command_id: null } });
      const control = new InterruptControl(deps);
      await control.interrupt();
      control.applyStatus("completed");
      await vi.advanceTimersByTimeAsync(30_000);
      expect(control.label).toBe("Прервано");
    } finally {
      vi.useRealTimers();
    }
  });

  it("reset_returns_the_idle_label_only_after_the_last_interrupt_ended", async () => {
    const deps: InterruptDeps = { conversationId: "conv-1", api: async () => ({ requested: { interrupt_command_id: "c1", withdraw_command_id: null } }) };
    const control = new InterruptControl(deps);
    await control.interrupt();
    control.reset();
    expect(control.label).toBe("Прерываю…");
    control.applyStatus("completed");
    control.reset();
    expect(control.label).toBe("Прервать");
  });
});
