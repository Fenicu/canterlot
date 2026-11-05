import { describe, it, expect, vi } from "vitest";
import { DraftSync, type DraftRecord } from "#lib/drafts/sync.js";

interface Timer {
  at: number;
  fn: () => void;
  cancelled: boolean;
}

function harness(options: {
  online?: boolean;
  local?: { text: string; updatedAt: number; attachments?: { id: string }[] } | null;
} = {}) {
  let now = 0;
  let current = "";
  let revision = 1;
  const timers: Timer[] = [];
  const puts: Array<{ text: string; base: number }> = [];
  const applied: string[] = [];
  const localSets: string[] = [];
  let local = options.local ?? null;
  let online = options.online ?? true;

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
    online: () => online,
    current: () => current,
    apply: (text) => {
      current = text;
      applied.push(text);
    },
    local: {
      load: async () => local,
      set: (text) => {
        localSets.push(text);
        local = { text, updatedAt: now, attachments: local?.attachments };
      },
      clear: () => {
        local = null;
      },
    },
    api: {
      put: async (text, base) => {
        puts.push({ text, base });
        const record: DraftRecord = {
          text,
          revision: revision++,
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
    await Promise.resolve();
    await Promise.resolve();
  }

  return {
    sync,
    puts,
    applied,
    localSets,
    advance,
    setText(text: string) {
      current = text;
    },
    text: () => current,
    setOnline(value: boolean) {
      online = value;
    },
    local: () => local,
  };
}

describe("drafts/sync", () => {
  it("saves_500ms_after_the_last_keystroke", async () => {
    const h = harness();
    await h.sync.start(null);
    h.setText("привет");
    h.sync.typed();
    await h.advance(499);
    expect(h.puts).toEqual([]);
    await h.advance(1);
    expect(h.puts).toEqual([{ text: "привет", base: 0 }]);
  });

  it("does_not_send_when_the_text_did_not_change", async () => {
    const h = harness();
    await h.sync.start({ text: "то же", revision: 4 });
    h.sync.typed();
    await h.advance(500);
    expect(h.puts).toEqual([]);
    expect(h.sync.dirty).toBe(false);
  });

  it("ignores_the_echo_of_this_device", async () => {
    const h = harness();
    await h.sync.start({ text: "", revision: 1 });
    h.applied.length = 0;
    h.setText("моё");
    h.sync.remote({ text: "моё", revision: 2, device_id: "device-a" });
    expect(h.text()).toBe("моё");
    expect(h.sync.revision).toBe(1);
    expect(h.applied).toEqual([]);
  });

  it("applies_another_device_at_once_when_there_are_no_local_edits", async () => {
    const h = harness();
    await h.sync.start({ text: "было", revision: 1 });
    h.sync.remote({ text: "с телефона", revision: 2, device_id: "device-b" });
    expect(h.text()).toBe("с телефона");
    expect(h.sync.revision).toBe(2);
  });

  it("waits_for_a_two_second_pause_before_replacing_active_typing", async () => {
    const h = harness();
    await h.sync.start({ text: "", revision: 1 });
    h.setText("печатаю");
    h.sync.typed();
    await h.advance(100);
    h.sync.remote({ text: "с другого", revision: 2, device_id: "device-b" });
    expect(h.text()).toBe("печатаю");
    await h.advance(1_899);
    expect(h.text()).toBe("печатаю");
    await h.advance(1);
    expect(h.text()).toBe("с другого");
  });

  it("a_new_keystroke_postpones_the_pending_remote_text", async () => {
    const h = harness();
    await h.sync.start({ text: "", revision: 1 });
    h.setText("а");
    h.sync.typed();
    await h.advance(100);
    h.sync.remote({ text: "чужое", revision: 2, device_id: "device-b" });
    await h.advance(1_500);
    h.setText("аб");
    h.sync.typed();
    await h.advance(1_900);
    expect(h.text()).toBe("аб");
    await h.advance(100);
    expect(h.text()).toBe("чужое");
  });

  it("keeps_an_offline_edit_in_the_local_buffer_and_sends_it_when_online", async () => {
    const h = harness({ online: false });
    await h.sync.start(null);
    h.setText("без сети");
    h.sync.typed();
    await h.advance(500);
    expect(h.puts).toEqual([]);
    expect(h.sync.dirty).toBe(true);
    expect(h.local()?.text).toBe("без сети");

    h.setOnline(true);
    await h.sync.flush();
    expect(h.puts).toEqual([{ text: "без сети", base: 0 }]);
    expect(h.sync.dirty).toBe(false);
  });

  it("keeps_a_newer_local_buffer_over_an_older_server_draft_and_sends_it", async () => {
    const h = harness({
      local: { text: "локальное", updatedAt: 2_000 },
    });
    await h.sync.start({
      text: "серверное",
      revision: 7,
      updated_at: new Date(1_000).toISOString(),
    });
    expect(h.text()).toBe("локальное");
    await h.advance(500);
    expect(h.puts).toEqual([{ text: "локальное", base: 7 }]);
  });

  it("takes_the_server_draft_when_it_is_newer_than_the_local_buffer", async () => {
    const h = harness({
      local: { text: "локальное", updatedAt: 1_000 },
    });
    await h.sync.start({
      text: "серверное",
      revision: 7,
      updated_at: new Date(2_000).toISOString(),
    });
    expect(h.text()).toBe("серверное");
    expect(h.sync.dirty).toBe(false);
    await h.advance(500);
    expect(h.puts).toEqual([]);
  });

  it("deleting_all_text_sends_an_empty_draft", async () => {
    const h = harness();
    await h.sync.start({ text: "было", revision: 3 });
    h.setText("");
    h.sync.typed();
    await h.advance(500);
    expect(h.puts).toEqual([{ text: "", base: 3 }]);
  });

  it("sent_stops_pending_saves_and_clears_the_local_buffer", async () => {
    const h = harness();
    await h.sync.start({ text: "ушло", revision: 2 });
    h.setText("ушло");
    h.sync.typed();
    h.sync.sent();
    await h.advance(500);
    expect(h.puts).toEqual([]);
    expect(h.sync.dirty).toBe(false);
    expect(h.local()).toBeNull();
  });
});
