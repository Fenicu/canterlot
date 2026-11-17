import { describe, it, expect } from "vitest";
import {
  DraftConflict,
  DraftSync,
  stopDraftSyncs,
  type DraftRecord,
} from "#lib/drafts/sync.js";

interface Timer {
  at: number;
  fn: () => void;
  cancelled: boolean;
}

function harness(options: {
  local: { text: string; baseRevision: number; unsynced: boolean };
  server: DraftRecord;
  conflict: DraftRecord;
}) {
  let now = 0;
  let current = "";
  const timers: Timer[] = [];
  const puts: Array<{ text: string; base: number }> = [];
  const applied: string[] = [];
  let local = { ...options.local, updatedAt: 1_000 };
  let conflict = options.conflict;
  let saveRevision = 100;

  const sync = new DraftSync({
    deviceId: "device-a",
    now: () => now,
    schedule: (fn, ms) => {
      const timer = { at: now + ms, fn, cancelled: false };
      timers.push(timer);
      return () => {
        timer.cancelled = true;
      };
    },
    online: () => true,
    current: () => current,
    currentAttachments: () => [],
    apply: (text) => {
      current = text;
      applied.push(text);
    },
    local: {
      load: async () => local,
      set: (text, edit) => {
        local = {
          text,
          baseRevision: edit?.baseRevision ?? local.baseRevision,
          unsynced: edit?.unsynced ?? false,
          updatedAt: now,
        };
      },
      clear: () => {
        local = { text: "", baseRevision: 0, unsynced: false, updatedAt: now };
      },
    },
    api: {
      put: async (_text, _attachments, base) => {
        puts.push({ text: _text, base });
        if (conflict) {
          const record = conflict;
          conflict = null as unknown as DraftRecord;
          throw new DraftConflict(record);
        }
        const record: DraftRecord = {
          text: _text,
          attachment_ids: [],
          revision: saveRevision++,
          updated_at: new Date(now).toISOString(),
          device_id: "device-a",
        };
        return record;
      },
    },
  });

  async function advance(ms: number) {
    const target = now + ms;
    for (;;) {
      timers.sort((a, b) => a.at - b.at);
      const next = timers.find((timer) => !timer.cancelled && timer.at <= target);
      if (!next) break;
      next.cancelled = true;
      now = next.at;
      next.fn();
      await Promise.resolve();
      await Promise.resolve();
    }
    now = target;
  }

  return { sync, server: options.server, puts, applied, advance, text: () => current, local: () => local };
}

describe("drafts/conflict", () => {
  it("a_conflict_rebases_a_real_offline_edit_onto_the_current_revision", async () => {
    const h = harness({
      local: { text: "набрано без сети", baseRevision: 3, unsynced: true },
      server: { text: "", revision: 3 },
      conflict: { text: "новое с другого устройства", revision: 7 },
    });
    await h.sync.start(h.server);
    await h.advance(1_000);

    expect(h.puts).toEqual([
      { text: "набрано без сети", base: 3 },
      { text: "набрано без сети", base: 7 },
    ]);
    expect(h.text()).toBe("набрано без сети");
    expect(h.local()).toMatchObject({ baseRevision: 100, unsynced: false });
    stopDraftSyncs();
  });

  it("a_conflict_without_unsynced_edits_adopts_the_server_copy", async () => {
    const h = harness({
      local: { text: "тот же текст", baseRevision: 3, unsynced: true },
      server: { text: "", revision: 3 },
      conflict: { text: "тот же текст", revision: 7 },
    });
    await h.sync.start(h.server);
    await h.advance(1_000);

    expect(h.puts).toEqual([{ text: "тот же текст", base: 3 }]);
    expect(h.text()).toBe("тот же текст");
    expect(h.applied).toContain("тот же текст");
    expect(h.local()).toMatchObject({ baseRevision: 7, unsynced: false });
    stopDraftSyncs();
  });
});
