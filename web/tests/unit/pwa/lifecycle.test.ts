import { expect, test, vi } from "vitest";
import { IDBFactory, IDBKeyRange } from "fake-indexeddb";
import { openDb, retireStorage } from "../../../src/lib/storage/idb";
import { retireOwner } from "../../../src/lib/storage/owner";
import { DraftStore, type Draft } from "../../../src/lib/drafts/store";

test("signout_does_not_restore_a_draft_from_an_inflight_read", async () => {
  vi.stubGlobal("indexedDB", new IDBFactory());
  vi.stubGlobal("IDBKeyRange", IDBKeyRange);
  const db = await openDb("alice");
  let finish!: (draft: Draft) => void;
  vi.spyOn(db, "get").mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve as typeof finish;
      }),
  );
  const drafts = new DraftStore("alice", db);
  try {
    const loading = drafts.load("c");
    drafts.retire();
    finish({
      conversationId: "c",
      text: "Личный черновик",
      attachments: [],
      updatedAt: 1,
    });
    expect(await loading).toBeNull();
  } finally {
    drafts.retire();
    retireStorage();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  }
});

test("browser_runtime_rebinds_cleanup_after_an_owner_change_during_startup", async () => {
  vi.stubGlobal("indexedDB", new IDBFactory());
  vi.stubGlobal("IDBKeyRange", IDBKeyRange);
  const runtime = {
    db: await openDb("bob"),
    list: { onDeleted: () => () => undefined },
  };
  let current: typeof runtime | null = null;
  vi.doMock("#lib/app/runtime.js", () => ({
    getRuntime: () => {
      if (!current) throw new Error("runtime_missing");
      return current;
    },
    createRuntime: async () => {
      retireOwner();
      current = runtime;
      return runtime;
    },
  }));
  try {
    const { browserRuntime } = await import("../../../src/lib/app/browser");
    const { sessionUploads } = await import("../../../src/lib/screens/chat/open");
    await browserRuntime();
    const before = sessionUploads();
    retireOwner();
    const after = sessionUploads();
    expect(after).not.toBe(before);
    before.dispose();
    after.dispose();
  } finally {
    vi.doUnmock("#lib/app/runtime.js");
    retireOwner();
    retireStorage();
    vi.unstubAllGlobals();
  }
});
