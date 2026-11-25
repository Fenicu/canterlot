import { afterEach, describe, expect, it, vi } from "vitest";
import { createRawSnippet, flushSync, mount, unmount } from "svelte";
import type { ChatStore } from "#lib/stores/chat.svelte.js";
import { pane } from "#lib/shell/pane.svelte.js";

let planned = new Set<string>();
vi.mock("#lib/app/runtime.js", () => ({
  getRuntime: () => ({ list: { plan: { has: (id: string) => planned.has(id) } } }),
}));
vi.mock("#lib/screens/plan/PlanView.svelte", async () => ({ default: (await import("./support/PlanViewStub.svelte")).default }));

const { default: DelegationTabs } = await import("#lib/screens/chat/DelegationTabs.svelte");

const tree = createRawSnippet(() => ({ render: () => '<div data-stub-tree="">Дерево</div>' }));
const chat = { id: "orch" } as unknown as ChatStore;

describe("chat/DelegationTabs", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
    planned = new Set();
    pane.forChat("reset");
  });

  function show(props: Record<string, unknown> = {}): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(DelegationTabs, { target, props: { chat, children: tree, ...props } });
    flushSync();
    return target;
  }

  it("stacked_pane_shows_the_plan_above_the_tree_without_tabs", () => {
    planned = new Set(["orch"]);
    const target = show({ stacked: true });
    expect(target.querySelector('[data-role="delegation-tabs"]')).toBeNull();
    const plan = target.querySelector('[data-role="pane-plan"]')!;
    const delegates = target.querySelector('[data-role="pane-tree"]')!;
    expect(plan.querySelector("[data-stub-plan='orch']")).not.toBeNull();
    expect(delegates.querySelector("[data-stub-tree]")).not.toBeNull();
    expect(plan.compareDocumentPosition(delegates) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect([...target.querySelectorAll("h3")].map((node) => node.textContent)).toEqual(["План", "Делегаты"]);
  });

  it("stacked_pane_ignores_the_tab_picked_below_the_threshold", () => {
    planned = new Set(["orch"]);
    pane.forChat("orch");
    pane.openPlan();
    const target = show({ stacked: true });
    expect(target.querySelector("[data-stub-tree]")).not.toBeNull();
    expect(target.querySelector("[data-stub-plan]")).not.toBeNull();
  });

  it("stacked_pane_without_a_plan_is_just_the_tree", () => {
    const target = show({ stacked: true });
    expect(target.querySelector('[data-role="pane-plan"]')).toBeNull();
    expect(target.querySelector("h3")).toBeNull();
    expect(target.querySelector("[data-stub-tree]")).not.toBeNull();
  });

  it("below_the_threshold_the_tabs_stay_as_today", () => {
    planned = new Set(["orch"]);
    pane.forChat("orch");
    const target = show();
    expect(target.querySelector('[data-role="delegation-tabs"]')).not.toBeNull();
    expect(target.querySelector("[data-stub-tree]")).not.toBeNull();
    expect(target.querySelector("[data-stub-plan]")).toBeNull();
    pane.openPlan();
    flushSync();
    expect(target.querySelector('[data-role="plan-tab"] [data-stub-plan]')).not.toBeNull();
    expect(target.querySelector("[data-stub-tree]")).toBeNull();
  });
});
