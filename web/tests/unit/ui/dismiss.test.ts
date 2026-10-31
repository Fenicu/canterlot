import { afterEach, describe, expect, it, vi } from "vitest";
import { dismissable } from "#lib/ui/dismiss.js";

function press(target: Element): void {
  target.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, composed: true }));
}

describe("ui/dismissable", () => {
  afterEach(() => {
    document.body.textContent = "";
  });

  function setup(keep?: (target: Element) => boolean) {
    const panel = document.createElement("div");
    const inner = document.createElement("input");
    const outside = document.createElement("button");
    panel.append(inner);
    document.body.append(panel, outside);
    const onclose = vi.fn();
    const action = dismissable(panel, { onclose, keep });
    return { panel, inner, outside, onclose, action };
  }

  it("closes_on_a_press_outside_and_not_inside", () => {
    const { inner, outside, onclose, action } = setup();
    press(inner);
    expect(onclose).not.toHaveBeenCalled();
    press(outside);
    expect(onclose).toHaveBeenCalledWith("outside");
    action.destroy();
  });

  it("leaves_its_toggle_to_the_toggle", () => {
    const { outside, onclose, action } = setup((target) => target === outside);
    press(outside);
    expect(onclose).not.toHaveBeenCalled();
    action.destroy();
  });

  it("modal_sheet_above_keeps_its_presses_and_its_escape", () => {
    const { onclose, action } = setup();
    const modal = document.createElement("dialog");
    const field = document.createElement("input");
    modal.append(field);
    modal.setAttribute("open", "");
    document.body.append(modal);
    press(field);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(onclose).not.toHaveBeenCalled();
    modal.remove();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(onclose).toHaveBeenCalledWith("escape");
    action.destroy();
  });

  it("stops_listening_when_gone", () => {
    const { outside, onclose, action } = setup();
    action.destroy();
    press(outside);
    expect(onclose).not.toHaveBeenCalled();
  });
});
