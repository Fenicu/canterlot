import { describe, it, expect } from "vitest";
import {
  decideEnter,
  isTouchPrimary,
  setPointerTypeForTest,
  turnIsLive,
  type KeyCtx,
} from "#lib/composer/sendRules.js";
import {
  getEnterPref,
  setEnterPref,
  ENTER_PREF_KEY,
} from "#lib/composer/prefs.js";

describe("composer/sendRules", () => {
  it("decide_enter_table", () => {
    // Table of scenarios: pointer x pref x modifiers x isComposing x keyCode x key
    const table: Array<{
      ctx: Partial<KeyCtx>;
      expected: "newline" | "send" | "send_later_menu" | "ignore";
      desc: string;
    }> = [
      // 1. Touch pointer: Enter always inserts newline, never sends (Review Focus 2)
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "auto",
          shiftKey: false,
        },
        expected: "newline",
        desc: "touch auto Enter -> newline",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "auto",
          shiftKey: true,
        },
        expected: "newline",
        desc: "touch auto Shift+Enter -> newline",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "newline",
          shiftKey: false,
        },
        expected: "newline",
        desc: "touch newline Enter -> newline",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "send",
          shiftKey: false,
        },
        expected: "send",
        desc: "touch explicit pref send Enter -> send",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "send",
          shiftKey: true,
        },
        expected: "newline",
        desc: "touch explicit pref send Shift+Enter -> newline",
      },

      // 2. Desktop (mouse/unknown): auto sends on Enter, newline on Shift+Enter
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "auto",
          shiftKey: false,
        },
        expected: "send",
        desc: "mouse auto Enter -> send",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "auto",
          shiftKey: true,
        },
        expected: "newline",
        desc: "mouse auto Shift+Enter -> newline",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "unknown",
          pref: "auto",
          shiftKey: false,
        },
        expected: "send",
        desc: "unknown auto Enter -> send",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "unknown",
          pref: "auto",
          shiftKey: true,
        },
        expected: "newline",
        desc: "unknown auto Shift+Enter -> newline",
      },

      // 3. Desktop with pref newline
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "newline",
          shiftKey: false,
        },
        expected: "newline",
        desc: "mouse pref newline Enter -> newline",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "newline",
          shiftKey: true,
        },
        expected: "newline",
        desc: "mouse pref newline Shift+Enter -> newline",
      },

      // 4. Ctrl/Cmd+Shift+Enter -> send_later_menu
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "auto",
          ctrlKey: true,
          shiftKey: true,
        },
        expected: "send_later_menu",
        desc: "Ctrl+Shift+Enter -> send_later_menu",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "mouse",
          pref: "auto",
          metaKey: true,
          shiftKey: true,
        },
        expected: "send_later_menu",
        desc: "Cmd+Shift+Enter -> send_later_menu",
      },
      {
        ctx: {
          key: "Enter",
          keyCode: 13,
          pointer: "touch",
          pref: "auto",
          ctrlKey: true,
          shiftKey: true,
        },
        expected: "send_later_menu",
        desc: "touch Ctrl+Shift+Enter -> send_later_menu",
      },

      // 5. Non-enter keys -> ignore
      {
        ctx: { key: "a", keyCode: 65, pointer: "mouse", pref: "auto" },
        expected: "ignore",
        desc: 'key "a" -> ignore',
      },
      {
        ctx: { key: "Tab", keyCode: 9, pointer: "mouse", pref: "auto" },
        expected: "ignore",
        desc: "key Tab -> ignore",
      },
    ];

    const defaults: KeyCtx = {
      key: "Enter",
      shiftKey: false,
      ctrlKey: false,
      metaKey: false,
      altKey: false,
      isComposing: false,
      keyCode: 13,
      pointer: "mouse",
      pref: "auto",
    };

    for (const row of table) {
      const ctx: KeyCtx = { ...defaults, ...row.ctx };
      const res = decideEnter(ctx);
      expect(res, row.desc).toBe(row.expected);
    }
  });

  it("ime_composition_enter_never_sends", () => {
    // Review Focus 2: IME composition (isComposing or keyCode 229) is always ignored
    const defaults: KeyCtx = {
      key: "Enter",
      shiftKey: false,
      ctrlKey: false,
      metaKey: false,
      altKey: false,
      isComposing: true,
      keyCode: 13,
      pointer: "mouse",
      pref: "send",
    };

    // 1. isComposing: true -> ignore
    expect(decideEnter(defaults)).toBe("ignore");

    // 2. keyCode: 229 (Android/Samsung IME) -> ignore
    expect(
      decideEnter({
        ...defaults,
        isComposing: false,
        keyCode: 229,
      }),
    ).toBe("ignore");

    // 3. Both isComposing and keyCode 229 -> ignore
    expect(
      decideEnter({
        ...defaults,
        isComposing: true,
        keyCode: 229,
      }),
    ).toBe("ignore");

    // 4. Even with Ctrl/Cmd+Shift+Enter, composing is ignored
    expect(
      decideEnter({
        ...defaults,
        ctrlKey: true,
        shiftKey: true,
        isComposing: true,
      }),
    ).toBe("ignore");
  });

  it("prefs_and_pointer_detection_work_as_expected", () => {
    localStorage.clear();
    expect(getEnterPref()).toBe("auto");

    setEnterPref("newline");
    expect(getEnterPref()).toBe("newline");
    expect(localStorage.getItem(ENTER_PREF_KEY)).toBe("newline");

    setEnterPref("send");
    expect(getEnterPref()).toBe("send");

    setPointerTypeForTest("touch");
    expect(isTouchPrimary()).toBe(true);

    setPointerTypeForTest("mouse");
    expect(isTouchPrimary()).toBe(false);
  });

  it("turn_is_live_with_the_slot_or_in_a_model_phase_of_a_wake_turn", () => {
    const running = { running: { input_id: "i" } };
    const activity = (state: string, phase?: string) => ({ state, evidence: { phase } });
    expect(turnIsLive(activity("working"), running)).toBe(true);
    expect(turnIsLive(activity("stalled"), running)).toBe(true);
    expect(turnIsLive(activity("working", "generating"), "free")).toBe(true);
    expect(turnIsLive(activity("working", "running_tool"), "free")).toBe(true);
    expect(turnIsLive(activity("working", "context_maintenance"), "free")).toBe(true);
    expect(turnIsLive(activity("stalled", "generating"), "free")).toBe(true);
    expect(turnIsLive(activity("working", "starting"), "free")).toBe(false);
    expect(turnIsLive(activity("working", "stopping"), "free")).toBe(false);
    expect(turnIsLive(activity("working", "awaiting_input"), { dispatched: {} })).toBe(false);
    expect(turnIsLive(activity("idle", "generating"), "free")).toBe(false);
    expect(turnIsLive(activity("waiting_for_user", "generating"), running)).toBe(false);
    expect(turnIsLive(null, running)).toBe(false);
  });
});
