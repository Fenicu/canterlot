import "fake-indexeddb/auto";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { DraftStore } from "#lib/drafts/store.js";
import { openDb } from "#lib/storage/idb.js";

describe("drafts/store", () => {
  const ownerId = "alice";

  beforeEach(async () => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("draft_is_debounced_flushes_on_hide_and_mirrors_to_local_storage_on_pagehide", async () => {
    const db = await openDb(ownerId);
    const store = new DraftStore(ownerId, db, { debounceMs: 50 });

    store.set("c-1", { text: "Hello world" });

    // Right away, not yet written to IDB (debounced 50ms)
    let inDb = await db.get<{ text: string }>("drafts", "c-1");
    expect(inDb).toBeUndefined();

    // After 10ms - still not in DB
    await new Promise((r) => setTimeout(r, 10));
    inDb = await db.get<{ text: string }>("drafts", "c-1");
    expect(inDb).toBeUndefined();

    // Trigger visibilitychange to 'hidden' -> must flush immediately
    Object.defineProperty(document, "visibilityState", {
      value: "hidden",
      configurable: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));
    await new Promise((r) => setTimeout(r, 10));

    inDb = await db.get<{ text: string }>("drafts", "c-1");
    expect(inDb?.text).toBe("Hello world");

    // Update again and trigger pagehide -> must mirror to localStorage
    store.set("c-1", { text: "Unsaved last second text" });
    window.dispatchEvent(new Event("pagehide"));

    const mirrored = localStorage.getItem("canterlot.draft-tail.c-1");
    expect(mirrored).toBeTruthy();
    const parsed = JSON.parse(mirrored!);
    expect(parsed.text).toBe("Unsaved last second text");

    store.destroy();
  });

  it("draft_is_scoped_by_owner_and_conversation", async () => {
    const dbAlice = await openDb("alice");
    const dbBob = await openDb("bob");
    const storeAlice = new DraftStore("alice", dbAlice);
    const storeBob = new DraftStore("bob", dbBob);

    storeAlice.set("c-1", {
      text: "Alice draft 1",
      attachments: [{ id: "a1" }],
    });
    storeAlice.set("c-2", { text: "Alice draft 2", attachments: [] });
    storeBob.set("c-1", { text: "Bob draft 1", attachments: [{ id: "b1" }] });
    await storeAlice.flush();
    await storeBob.flush();

    const alice1 = await storeAlice.load("c-1");
    const alice2 = await storeAlice.load("c-2");
    const bob1 = await storeBob.load("c-1");

    expect(alice1?.text).toBe("Alice draft 1");
    expect(alice1?.attachments).toEqual([{ id: "a1" }]);
    expect(alice2?.text).toBe("Alice draft 2");
    expect(bob1?.text).toBe("Bob draft 1");
    expect(bob1?.attachments).toEqual([{ id: "b1" }]);

    storeAlice.destroy();
    storeBob.destroy();
  });

  it("mirror_is_consumed_once_and_newer_wins", async () => {
    const db = await openDb(ownerId);

    // IDB has an older draft (timestamp 1000)
    await db.put("drafts", "c-1", {
      conversationId: "c-1",
      text: "Older draft in IDB",
      attachments: [],
      updatedAt: 1000,
    });

    localStorage.setItem(
      "canterlot.draft-tail.c-1",
      JSON.stringify({
        ownerId,
        conversationId: "c-1",
        text: "Newer draft in localStorage",
        attachments: [{ id: "att-new" }],
        updatedAt: 2000,
      }),
    );

    const store = new DraftStore(ownerId, db);
    const loaded = await store.load("c-1");

    expect(loaded?.text).toBe("Newer draft in localStorage");
    expect(loaded?.attachments).toEqual([{ id: "att-new" }]);

    expect(localStorage.getItem("canterlot.draft-tail.c-1")).toBeNull();

    // And written to IDB
    const inDb = await db.get<{ text: string }>("drafts", "c-1");
    expect(inDb?.text).toBe("Newer draft in localStorage");

    store.destroy();
  });

  it("draft_persistence_lag_is_measured", async () => {
    const db = await openDb(ownerId);
    let measuredLag = -1;
    const store = new DraftStore(ownerId, db, {
      onPersistLag: (lagMs) => {
        measuredLag = lagMs;
      },
    });

    store.set("c-1", { text: "Measuring lag" });
    await store.flush();

    expect(measuredLag).toBeGreaterThanOrEqual(0);
    expect(store.lag.lastMs).toBe(measuredLag);
    expect(store.lag.maxMs).toBeGreaterThanOrEqual(store.lag.lastMs);
    store.destroy();
  });

  it("pagehide_mirrors_a_draft_while_its_write_is_still_in_flight", async () => {
    const db = await openDb(ownerId);
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const realPut = db.put.bind(db);
    db.put = async (store, key, value) => {
      await gate;
      return realPut(store, key, value);
    };
    const store = new DraftStore(ownerId, db, { debounceMs: 60_000 });
    store.set("c-1", { text: "tail", attachments: [{ id: "a1" }] });
    const flushing = store.flush();
    window.dispatchEvent(new Event("pagehide"));
    const raw = localStorage.getItem("canterlot.draft-tail.c-1");
    expect(raw).toBeTruthy();
    const mirrored = JSON.parse(raw!);
    expect(mirrored.text).toBe("tail");
    expect(mirrored.ownerId).toBe(ownerId);
    release();
    await flushing;
    store.destroy();
  });

  it("partial_draft_update_keeps_fields_already_stored", async () => {
    const db = await openDb(ownerId);
    const store = new DraftStore(ownerId, db, { debounceMs: 0 });
    store.set("c-1", { text: "hello", attachments: [{ id: "a1" }] });
    await store.flush();
    store.set("c-1", { text: "hello!" });
    await store.flush();
    const loaded = await store.load("c-1");
    expect(loaded?.text).toBe("hello!");
    expect(loaded?.attachments).toEqual([{ id: "a1" }]);
    store.set("c-1", { attachments: [{ id: "a1" }, { id: "a2" }] });
    await store.flush();
    const again = await store.load("c-1");
    expect(again?.text).toBe("hello!");
    expect(again?.attachments).toEqual([{ id: "a1" }, { id: "a2" }]);
    store.destroy();
  });

  it("default_debounce_is_250ms", async () => {
    vi.useRealTimers();
    const db = await openDb(ownerId);
    const store = new DraftStore(ownerId, db);
    store.set("c-debounce", { text: "later" });
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(
      (await db.get<{ text: string }>("drafts", "c-debounce"))?.text,
    ).not.toBe("later");
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect((await db.get<{ text: string }>("drafts", "c-debounce"))?.text).toBe(
      "later",
    );
    store.destroy();
  });

  it("freeze_and_composer_blur_flush_the_draft", async () => {
    const db = await openDb(ownerId);
    const store = new DraftStore(ownerId, db, { debounceMs: 60_000 });
    store.set("c-1", { text: "frozen" });
    window.dispatchEvent(new Event("freeze"));
    await vi.waitFor(async () =>
      expect((await db.get<{ text: string }>("drafts", "c-1"))?.text).toBe(
        "frozen",
      ),
    );

    store.set("c-1", { text: "blurred" });
    const composer = new EventTarget();
    const stop = store.watchComposer(composer);
    composer.dispatchEvent(new Event("blur"));
    await vi.waitFor(async () =>
      expect((await db.get<{ text: string }>("drafts", "c-1"))?.text).toBe(
        "blurred",
      ),
    );
    stop();

    store.set("c-1", { text: "left" });
    await store.leave();
    expect((await db.get<{ text: string }>("drafts", "c-1"))?.text).toBe(
      "left",
    );
    store.destroy();
  });

  it("load_keeps_the_newer_indexeddb_draft", async () => {
    const db = await openDb(ownerId);
    await db.put("drafts", "c-1", {
      ownerId,
      conversationId: "c-1",
      text: "from indexeddb",
      attachments: [],
      updatedAt: 3000,
    });
    localStorage.setItem(
      "canterlot.draft-tail.c-1",
      JSON.stringify({
        ownerId,
        conversationId: "c-1",
        text: "from mirror",
        attachments: [],
        updatedAt: 1000,
      }),
    );
    const store = new DraftStore(ownerId, db);
    const loaded = await store.load("c-1");
    expect(loaded?.text).toBe("from indexeddb");
    expect(localStorage.getItem("canterlot.draft-tail.c-1")).toBeNull();
    store.destroy();
  });

  it("lag_measures_from_the_last_edit", async () => {
    const db = await openDb(ownerId);
    const store = new DraftStore(ownerId, db, { debounceMs: 80 });
    store.set("c-lag", { text: "a" });
    await new Promise((resolve) => setTimeout(resolve, 70));
    store.set("c-lag", { text: "ab" });
    await vi.waitFor(() => expect(store.lag.lastMs).toBeGreaterThan(0));
    expect(store.lag.lastMs).toBeGreaterThanOrEqual(70);
    expect(store.lag.lastMs).toBeLessThan(130);
    store.destroy();
  });
});
