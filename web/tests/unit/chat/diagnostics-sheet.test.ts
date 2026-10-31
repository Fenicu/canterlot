import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import Diagnostics from "#lib/screens/chat/Diagnostics.svelte";
import { buildDiagnosticsReport } from "#lib/screens/chat/diagnostics.js";
import type { ChatStore } from "#lib/stores/chat.svelte.js";

vi.mock("#lib/ui/Sheet.svelte", async () => ({
  default: (await import("./support/SheetStub.svelte")).default,
}));

function chatWith(diagnostics: unknown[]): ChatStore {
  return {
    id: "conv-1",
    detail: { title: "Сессия", machine: { name: "workstation" } },
    execution: null,
    activity: null,
    cursor: "10",
    status: "live",
    diagnostics,
  } as unknown as ChatStore;
}

function adapterDiagnostic(payload: Record<string, unknown>) {
  return {
    kind: "adapter_diagnostic",
    correlation_id: "corr-1",
    scope_seq: "7",
    payload: { kind: "unknown_optional_event", ...payload },
  };
}

describe("chat/Diagnostics", () => {
  let instance: ReturnType<typeof mount> | undefined;

  afterEach(() => {
    if (instance) void unmount(instance);
    instance = undefined;
    document.body.textContent = "";
  });

  function show(diagnostics: unknown[]): HTMLElement {
    const target = document.createElement("div");
    document.body.appendChild(target);
    instance = mount(Diagnostics, {
      target,
      props: { chat: chatWith(diagnostics), onclose: () => undefined },
    });
    flushSync();
    return target;
  }

  it("field_names_of_an_unknown_frame_are_listed_under_its_event", () => {
    const target = show([
      adapterDiagnostic({
        event_type: "thread_futureThing",
        field_names: ["apiKey", "nested", "threadId"],
      }),
    ]);
    const fields = target.querySelector(".fields");
    expect(fields?.textContent).toContain("Поля кадра");
    expect(fields?.textContent).toContain("apiKey, nested, threadId");
    expect(target.textContent).toContain("thread_futureThing");
  });

  it("events_without_field_names_show_no_field_line", () => {
    const target = show([
      adapterDiagnostic({ event_type: "old_worker" }),
      adapterDiagnostic({ event_type: "empty", field_names: [] }),
    ]);
    expect(target.querySelector(".fields")).toBeNull();
    expect(target.textContent).toContain("old_worker");
  });

  it("copied_report_carries_the_names_and_nothing_else", () => {
    const report = buildDiagnosticsReport({
      conversationId: "c1",
      diagnosticEvents: [
        {
          kind: "adapter_diagnostic",
          detail: "thread_futureThing",
          fields: ["apiKey", "threadId"],
          seq: "7",
        },
        { kind: "adapter_diagnostic", detail: "old_worker", seq: "8" },
      ],
    });
    expect(report).toContain("fields: apiKey, threadId");
    expect(report.match(/fields:/g)).toHaveLength(1);
  });
});
