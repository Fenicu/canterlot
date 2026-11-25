import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import LiveLine from "#lib/timeline/LiveLine.svelte";

const NOW = 1_000_000;

describe("timeline/LiveLine", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function render(props: { phase?: string; tool?: string; quiet?: boolean }): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(LiveLine, {
      target,
      props: { startedAt: NOW - 65_000, now: NOW, ...props },
    });
    flushSync();
    return target;
  }

  it("names_the_phase_and_keeps_the_clock_with_it", () => {
    const line = render({ phase: "tool", tool: "Bash" }).querySelector("[data-working]")!;
    const label = line.querySelector(".label")!;
    const tail = line.querySelector(".tail")!;

    expect(line.textContent).toContain("Выполняет Bash · 1 мин 5 с");
    // Разделитель — часть подписи: пока подпись сменяется, «· 1 мин 5 с» без слова не встаёт.
    expect(label.textContent).toBe("Выполняет Bash ·");
    expect(tail.textContent).toBe(" 1 мин 5 с");
    expect(tail.textContent).not.toContain("·");
  });

  it("quiet_line_hides_the_whole_text_with_its_separator", () => {
    const line = render({ phase: "thinking", quiet: true }).querySelector("[data-working]")!;

    expect(line.hasAttribute("data-quiet")).toBe(true);
    expect(line.querySelector(".text")?.classList.contains("sr-only")).toBe(true);
  });
});
