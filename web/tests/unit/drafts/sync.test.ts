import { describe, it, expect } from "vitest";
import { DraftSync, stopDraftSyncs, type DraftRecord } from "#lib/drafts/sync.js";

interface Timer {
  at: number;
  fn: () => void;
  cancelled: boolean;
}

interface LocalState {
  text: string;
  updatedAt: number;
  attachments?: { id: string }[];
  baseRevision?: number;
  unsynced?: boolean;
}

function harness(options: {
  online?: boolean;
  local?: LocalState | null;
} = {}) {
  let now = 0;
  let current = "";
  let currentAttachments: string[] = [];
  const timers: Timer[] = [];
  const puts: Array<{ text: string; attachments: string[]; base: number }> = [];
  const applied: Array<{ text: string; attachments: string[] }> = [];
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
    currentAttachments: () => currentAttachments,
    apply: (text, attachments) => {
      current = text;
      currentAttachments = attachments;
      applied.push({ text, attachments });
    },
    local: {
      load: async () => local,
      set: (text, edit) => {
        localSets.push(text);
        local = {
          text,
          updatedAt: now,
          attachments: edit?.attachments ?? local?.attachments,
          baseRevision: edit?.baseRevision ?? local?.baseRevision,
          unsynced: edit?.unsynced ?? false,
        };
      },
      clear: () => {
        local = null;
      },
    },
    api: {
      put: async (text, attachments, base) => {
        puts.push({ text, attachments, base });
        // Как хаб: запись принимается на текущей ревизии и получает следующую.
        const record: DraftRecord = {
          text,
          attachment_ids: attachments,
          revision: base + 1,
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
    setAttachments(ids: string[]) {
      currentAttachments = ids;
    },
    text: () => current,
    attachments: () => currentAttachments,
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
    expect(h.puts).toEqual([{ text: "привет", attachments: [], base: 0 }]);
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

  // Чужая правка ждёт паузы, только пока своя запись не легла поверх неё: здесь сети нет.
  it("waits_for_a_two_second_pause_before_replacing_active_typing", async () => {
    const h = harness({ online: false });
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
    const h = harness({ online: false });
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

  it("a_deferred_remote_text_does_not_replace_a_newer_saved_revision", async () => {
    const h = harness();
    await h.sync.start({ text: "", revision: 3 });
    h.setText("важный длинный текст");
    h.sync.typed();
    // В ту же секунду с другого устройства приходит ревизия 4: владелец печатает, она ждёт паузы.
    h.sync.remote({ text: "с ноутбука", revision: 4, device_id: "device-b" });
    // Через 500 мс своя запись ложится поверх неё: хаб принимает ревизию 5.
    await h.advance(500);
    expect(h.puts).toEqual([{ text: "важный длинный текст", attachments: [], base: 4 }]);
    expect(h.sync.revision).toBe(5);
    // Пауза прошла: отложенная ревизия 4 старше сохранённой и в поле не встаёт.
    await h.advance(5_000);
    expect(h.text()).toBe("важный длинный текст");
    expect(h.sync.dirty).toBe(false);
    expect(h.local()).toMatchObject({ text: "важный длинный текст", baseRevision: 5, unsynced: false });
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
    expect(h.puts).toEqual([{ text: "без сети", attachments: [], base: 0 }]);
    expect(h.sync.dirty).toBe(false);
  });

  it("keeps_a_real_offline_edit_over_an_older_server_draft_and_sends_it", async () => {
    const h = harness({
      local: { text: "локальное", updatedAt: 2_000, baseRevision: 7, unsynced: true },
    });
    await h.sync.start({
      text: "серверное",
      revision: 7,
      updated_at: new Date(1_000).toISOString(),
    });
    expect(h.text()).toBe("локальное");
    await h.advance(500);
    expect(h.puts).toEqual([{ text: "локальное", attachments: [], base: 7 }]);
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
    expect(h.puts).toEqual([{ text: "", attachments: [], base: 3 }]);
  });

  it("saves_attachment_ids_with_the_text", async () => {
    const h = harness();
    await h.sync.start({ text: "", attachment_ids: [], revision: 2 });
    h.setText("с файлом");
    h.setAttachments(["a1", "a2"]);
    h.sync.typed();
    await h.advance(500);
    expect(h.puts).toEqual([
      { text: "с файлом", attachments: ["a1", "a2"], base: 2 },
    ]);
  });

  it("applies_remote_attachment_ids_and_clears_them_when_removed", async () => {
    const h = harness();
    await h.sync.start({ text: "было", attachment_ids: [], revision: 1 });
    h.sync.remote({
      text: "было",
      attachment_ids: ["a1"],
      revision: 2,
      device_id: "device-b",
    });
    expect(h.attachments()).toEqual(["a1"]);
    h.sync.remote({
      text: "было",
      attachment_ids: [],
      revision: 3,
      device_id: "device-b",
    });
    expect(h.attachments()).toEqual([]);
  });

  it("an_empty_draft_with_attachments_is_still_a_draft", async () => {
    const h = harness();
    await h.sync.start({ text: "", attachment_ids: [], revision: 1 });
    h.setAttachments(["a1"]);
    h.sync.typed();
    await h.advance(500);
    expect(h.puts).toEqual([
      { text: "", attachments: ["a1"], base: 1 },
    ]);
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

  it("stopped_sync_never_sends_the_emptied_field_to_the_hub", async () => {
    const h = harness();
    await h.sync.start({ text: "общий черновик", revision: 3 });
    h.setText("общий черновик и правка");
    h.sync.typed();
    stopDraftSyncs();
    h.setText("");
    h.sync.typed();
    await h.sync.leave();
    await h.advance(5_000);
    expect(h.puts).toEqual([]);
  });

  it("a_stale_synced_local_buffer_does_not_resurrect_text_when_the_server_has_no_draft", async () => {
    // Устройство S: давний локальный текст, черновик на хабе удалён отправкой с другого устройства.
    const h = harness({
      local: { text: "старый текст, отправленный несколько дней назад", updatedAt: 1_000 },
    });
    await h.sync.start(null);
    expect(h.text()).toBe("");
    await h.advance(5_000);
    expect(h.puts).toEqual([]);
    expect(h.local()?.text).toBe("");
  });

  it("an_empty_server_draft_wins_over_a_synced_local_buffer_even_with_a_newer_clock", async () => {
    // Событие draft_changed не несёт updated_at: сравнение по часам даёт serverTime = 0.
    const h = harness({
      local: { text: "старый текст", updatedAt: 10_000 },
    });
    await h.sync.start({ text: "", revision: 42, updated_at: null });
    expect(h.text()).toBe("");
    await h.advance(5_000);
    expect(h.puts).toEqual([]);
  });

  it("a_newer_server_revision_is_adopted_when_the_local_buffer_has_no_unsynced_edits", async () => {
    const h = harness({
      local: {
        text: "старый",
        updatedAt: 10_000,
        baseRevision: 3,
        unsynced: false,
      },
    });
    await h.sync.start({ text: "новый", revision: 9, updated_at: null });
    expect(h.text()).toBe("новый");
    await h.advance(5_000);
    expect(h.puts).toEqual([]);
  });

  it("a_real_offline_edit_is_kept_and_put_on_the_revision_it_was_edited_on", async () => {
    const h = harness({
      local: {
        text: "набрано без сети",
        updatedAt: 1_000,
        baseRevision: 3,
        unsynced: true,
      },
    });
    await h.sync.start({ text: "новый текст на хабе", revision: 9, updated_at: null });
    expect(h.text()).toBe("набрано без сети");
    await h.advance(500);
    expect(h.puts).toEqual([{ text: "набрано без сети", attachments: [], base: 3 }]);
  });
});
