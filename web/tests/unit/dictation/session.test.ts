import { describe, expect, it } from "vitest";
import { FakeClock } from "../../../testing/fake-hub/state.js";
import {
  DictationSession,
  dictationErrorText,
  type DictationCapture,
  type DictationDeps,
  type DictationSocket,
  type DictationView,
} from "#lib/dictation/session.js";

class FakeSocket implements DictationSocket {
  sent: (string | ArrayBuffer)[] = [];
  bufferedAmount = 0;
  closed = false;
  failFrames = 0;
  handlers!: { open(): void; message(data: string): void; close(code: number): void };
  send(data: string | ArrayBuffer): void {
    if (typeof data !== "string" && this.failFrames > 0) {
      this.failFrames -= 1;
      throw new Error("socket_closed");
    }
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  get texts(): Record<string, unknown>[] {
    return this.sent.filter((item): item is string => typeof item === "string").map((item) => JSON.parse(item));
  }
  get frames(): ArrayBuffer[] {
    return this.sent.filter((item): item is ArrayBuffer => typeof item !== "string");
  }
}

class FakeCapture implements DictationCapture {
  ready: Promise<void> = Promise.resolve();
  stopped = false;
  hooks!: { onFrame(frame: Uint8Array): void; onLost(): void };
  stop(): Promise<void> {
    this.stopped = true;
    return Promise.resolve();
  }
  frame(value = 4_000): void {
    const bytes = new Uint8Array(6);
    new DataView(bytes.buffer).setInt16(0, value, true);
    this.hooks.onFrame(bytes);
  }
}

function setup(options: { captureError?: Error; captureReady?: Promise<void> } = {}) {
  const clock = new FakeClock();
  const socket = new FakeSocket();
  const capture = new FakeCapture();
  if (options.captureError) capture.ready = Promise.reject(options.captureError);
  if (options.captureReady) capture.ready = options.captureReady;
  const views: DictationView[] = [];
  const inserts: { text: string; notice: string }[] = [];
  let unavailable = 0;
  const deps: DictationDeps = {
    connect: (handlers) => {
      socket.handlers = handlers;
      return socket;
    },
    startCapture: (hooks) => {
      capture.hooks = hooks;
      return capture;
    },
    schedule: clock.schedule,
    now: clock.now,
    client: "pwa/1.33.0",
    onview: (view) => views.push({ ...view }),
    oninsert: (text, notice) => inserts.push({ text, notice }),
    onunavailable: () => {
      unavailable += 1;
    },
  };
  const session = new DictationSession(deps);
  const last = () => views.at(-1)!;
  const state = (state: string, mode: string | null = null) =>
    socket.handlers.message(JSON.stringify({ type: "state", state, mode }));
  const text = (committed: string, tentative: string, seq: number) =>
    socket.handlers.message(JSON.stringify({ type: "text", committed, tentative, seq }));
  return {
    clock,
    socket,
    capture,
    session,
    views,
    inserts,
    last,
    state,
    text,
    get unavailable() {
      return unavailable;
    },
  };
}

function listing() {
  const harness = setup();
  return harness;
}

async function listening() {
  const harness = listing();
  await harness.session.start();
  harness.socket.handlers.open();
  harness.state("listening", "gpu");
  return harness;
}

describe("DictationSession start", () => {
  it("sends_the_frozen_start_message_and_names_the_client", async () => {
    const harness = listing();
    expect(harness.session.view.phase).toBe("starting");
    expect(harness.session.view.message).toBe("Подключаю микрофон…");
    await harness.session.start();
    harness.socket.handlers.open();
    expect(harness.socket.texts[0]).toEqual({
      type: "start",
      sample_rate: 16_000,
      format: "s16le",
      client: "pwa/1.33.0",
    });
    expect(harness.last()).toMatchObject({ phase: "loading", message: "Загружаю модель…" });
  });

  it("queues_frames_until_the_socket_opens", async () => {
    const harness = listing();
    await harness.session.start();
    harness.capture.frame();
    expect(harness.socket.frames).toHaveLength(0);
    harness.socket.handlers.open();
    expect(harness.socket.frames).toHaveLength(1);
    expect(harness.socket.texts[0]!.type).toBe("start");
  });

  it("shows_loading_until_the_hub_says_listening", async () => {
    const harness = listing();
    await harness.session.start();
    harness.socket.handlers.open();
    harness.state("loading");
    expect(harness.last()).toMatchObject({ phase: "loading", message: "Загружаю модель…" });
    harness.state("listening", "gpu");
    expect(harness.last()).toMatchObject({ phase: "listening", mode: "gpu", message: "Слушаю" });
  });

  it("names_the_slow_mode_on_cpu", async () => {
    const harness = listing();
    await harness.session.start();
    harness.socket.handlers.open();
    harness.state("listening", "cpu");
    expect(harness.last()).toMatchObject({ mode: "cpu", message: "Медленный режим" });
  });

  it("renders_only_the_latest_text_sequence", async () => {
    const harness = await listening();
    harness.text("Привет,", "это", 2);
    expect(harness.last()).toMatchObject({ committed: "Привет,", tentative: "это" });
    harness.text("старое", "", 1);
    expect(harness.last()).toMatchObject({ committed: "Привет,", tentative: "это" });
    harness.text("Привет, это", "", 3);
    expect(harness.last()).toMatchObject({ committed: "Привет, это", tentative: "" });
  });

  it("measures_the_input_level_from_frames", async () => {
    const harness = await listening();
    harness.capture.frame(30_000);
    expect(harness.last().level).toBeGreaterThan(0.5);
  });
});

describe("DictationSession pre-start queue", () => {
  it("flushes_forty_queued_frames_after_the_socket_opens", async () => {
    const harness = listing();
    await harness.session.start();
    for (let index = 0; index < 40; index += 1) harness.capture.frame();
    expect(harness.socket.frames).toHaveLength(0);
    harness.socket.handlers.open();
    expect(harness.socket.texts[0]!.type).toBe("start");
    expect(harness.socket.frames).toHaveLength(40);
    expect(harness.socket.sent[1]).toBeInstanceOf(ArrayBuffer);
  });

  it("ends_the_recording_instead_of_dropping_audio_when_the_queue_overflows", async () => {
    const harness = listing();
    await harness.session.start();
    for (let index = 0; index < 40; index += 1) harness.capture.frame();
    expect(harness.session.view.phase).toBe("loading");
    harness.capture.frame();
    expect(harness.session.view).toMatchObject({
      phase: "failed",
      message: "Связь прервалась",
      canInsert: false,
    });
    expect(harness.capture.stopped).toBe(true);
    expect(harness.socket.closed).toBe(true);
    // Позднее открытие сокета не отправляет ни start, ни накопленный звук.
    harness.socket.handlers.open();
    expect(harness.socket.texts).toHaveLength(0);
    expect(harness.socket.frames).toHaveLength(0);
    expect(harness.inserts).toHaveLength(0);
    expect(harness.session.view.phase).toBe("failed");
  });
});

describe("DictationSession finish", () => {
  it("stops_the_audio_graph_before_finish_and_inserts_the_final_once", async () => {
    const harness = await listening();
    harness.text("Привет", "", 1);
    harness.session.finish();
    expect(harness.session.view.phase).toBe("finishing");
    await harness.clock.advanceAsync(0);
    expect(harness.capture.stopped).toBe(true);
    expect(harness.socket.texts.at(-1)).toEqual({ type: "finish" });
    harness.socket.handlers.message(JSON.stringify({ type: "final", text: "Привет, мир", reason: "finish" }));
    expect(harness.inserts).toEqual([{ text: "Привет, мир", notice: "" }]);
    expect(harness.session.view.phase).toBe("closed");
    harness.socket.handlers.message(JSON.stringify({ type: "final", text: "ещё", reason: "finish" }));
    expect(harness.inserts).toHaveLength(1);
  });

  it("does_not_send_audio_after_finish_was_pressed", async () => {
    const harness = await listening();
    harness.session.finish();
    await harness.clock.advanceAsync(0);
    harness.capture.frame();
    expect(harness.socket.frames).toHaveLength(0);
  });

  it("waits_five_seconds_on_gpu_then_offers_the_partial_text", async () => {
    const harness = await listening();
    harness.text("Привет", " мир", 1);
    harness.session.finish();
    await harness.clock.advanceAsync(4_999);
    expect(harness.session.view.phase).toBe("finishing");
    await harness.clock.advanceAsync(1);
    expect(harness.session.view).toMatchObject({ phase: "review", canInsert: true });
    expect(harness.session.view.message).toContain("Не дождались");
    harness.session.insertPartial();
    expect(harness.inserts).toEqual([{ text: "Привет мир", notice: "" }]);
    expect(harness.session.view.phase).toBe("closed");
    harness.socket.handlers.message(JSON.stringify({ type: "final", text: "поздно", reason: "finish" }));
    expect(harness.inserts).toHaveLength(1);
  });

  it("waits_thirty_seconds_in_slow_mode", async () => {
    const harness = setup();
    await harness.session.start();
    harness.socket.handlers.open();
    harness.state("listening", "cpu");
    harness.text("Привет", "", 1);
    harness.session.finish();
    await harness.clock.advanceAsync(29_999);
    expect(harness.session.view.phase).toBe("finishing");
    await harness.clock.advanceAsync(1);
    expect(harness.session.view.phase).toBe("review");
  });

  it("offers_no_partial_insert_when_nothing_was_recognised", async () => {
    const harness = await listening();
    harness.session.finish();
    await harness.clock.advanceAsync(5_000);
    expect(harness.session.view).toMatchObject({ phase: "failed", canInsert: false });
    harness.session.insertPartial();
    expect(harness.inserts).toHaveLength(0);
  });

  it("discards_on_cancel_and_ignores_a_late_final", async () => {
    const harness = await listening();
    harness.text("Привет", "", 1);
    harness.session.cancel();
    expect(harness.socket.texts.at(-1)).toEqual({ type: "cancel" });
    expect(harness.socket.closed).toBe(true);
    expect(harness.capture.stopped).toBe(true);
    expect(harness.inserts).toHaveLength(0);
    expect(harness.session.view.phase).toBe("closed");
    harness.socket.handlers.message(JSON.stringify({ type: "final", text: "Привет", reason: "finish" }));
    expect(harness.inserts).toHaveLength(0);
  });

  it("inserts_a_too_long_final_with_a_notice", async () => {
    const harness = await listening();
    harness.text("Долгая", " речь", 1);
    harness.socket.handlers.message(
      JSON.stringify({ type: "final", text: "Долгая речь", reason: "too_long" }),
    );
    expect(harness.inserts).toEqual([{ text: "Долгая речь", notice: "Достигнут предел 10 минут" }]);
    expect(harness.capture.stopped).toBe(true);
    expect(harness.session.view.phase).toBe("closed");
  });
});

describe("DictationSession errors", () => {
  it("maps_every_error_code_to_russian", () => {
    expect(dictationErrorText("speech_unavailable")).toBe("Служба распознавания недоступна");
    expect(dictationErrorText("busy")).toBe("Диктовка уже идёт на другом устройстве");
    expect(dictationErrorText("replaced")).toBe("Диктовку начали на другом устройстве");
    expect(dictationErrorText("bad_request")).toBe("Хаб отклонил запрос диктовки");
    expect(dictationErrorText("bad_audio")).toBe("Хаб не принял звук");
    expect(dictationErrorText("idle")).toBe("Звук перестал поступать");
    expect(dictationErrorText("start_timeout")).toBe("Служба не успела загрузить модель");
    expect(dictationErrorText("lifetime")).toBe("Достигнут предел длительности диктовки");
    expect(dictationErrorText("engine_failed")).toBe("Сбой распознавания");
    expect(dictationErrorText("nonsense")).toBe("Диктовка прервалась из-за ошибки");
  });

  it("fails_with_the_mapped_message_when_no_audio_text_exists", async () => {
    const harness = await listening();
    harness.socket.handlers.message(JSON.stringify({ type: "error", code: "busy" }));
    expect(harness.session.view).toMatchObject({
      phase: "failed",
      message: "Диктовка уже идёт на другом устройстве",
      canInsert: false,
    });
    expect(harness.capture.stopped).toBe(true);
    expect(harness.inserts).toHaveLength(0);
  });

  it("keeps_the_partial_text_of_a_terminal_error", async () => {
    const harness = await listening();
    harness.capture.frame();
    harness.text("Привет", "", 1);
    harness.socket.handlers.message(JSON.stringify({ type: "error", code: "engine_failed" }));
    expect(harness.session.view).toMatchObject({ phase: "review", canInsert: true });
    harness.session.insertPartial();
    expect(harness.inserts).toEqual([{ text: "Привет", notice: "" }]);
  });

  it("asks_for_a_status_refresh_on_speech_unavailable", async () => {
    const harness = await listening();
    harness.socket.handlers.message(JSON.stringify({ type: "error", code: "speech_unavailable" }));
    expect(harness.unavailable).toBe(1);
    expect(harness.session.view.phase).toBe("failed");
  });

  it("offers_the_partial_text_when_the_connection_drops", async () => {
    const harness = await listening();
    harness.text("Привет", " мир", 1);
    harness.socket.handlers.close(1006);
    expect(harness.session.view).toMatchObject({
      phase: "review",
      canInsert: true,
      message: "Связь прервалась",
    });
    harness.session.insertPartial();
    expect(harness.inserts).toEqual([{ text: "Привет мир", notice: "" }]);
  });

  it("fails_cleanly_when_the_connection_drops_without_any_text", async () => {
    const harness = await listening();
    harness.socket.handlers.close(1001);
    expect(harness.session.view).toMatchObject({ phase: "failed", message: "Связь прервалась" });
  });

  it("ends_the_recording_through_the_finish_flow_on_a_lost_microphone", async () => {
    const harness = await listening();
    harness.text("Привет", "", 1);
    harness.capture.hooks.onLost();
    await harness.clock.advanceAsync(0);
    expect(harness.socket.texts.at(-1)).toEqual({ type: "finish" });
    harness.socket.handlers.message(JSON.stringify({ type: "final", text: "Привет", reason: "finish" }));
    expect(harness.inserts).toHaveLength(1);
  });

  it("ends_the_recording_when_a_frame_cannot_be_sent", async () => {
    const harness = await listening();
    harness.text("Привет", "", 1);
    harness.socket.failFrames = 1;
    harness.capture.frame();
    expect(harness.session.view).toMatchObject({
      phase: "review",
      canInsert: true,
      message: "Связь прервалась",
    });
    harness.session.insertPartial();
    expect(harness.inserts).toEqual([{ text: "Привет", notice: "" }]);
  });

  it("explains_a_denied_microphone", async () => {
    const harness = setup({ captureError: Object.assign(new Error("denied"), { name: "NotAllowedError" }) });
    await harness.session.start();
    expect(harness.session.view).toMatchObject({ phase: "failed", mic: "denied" });
    expect(harness.session.view.message).toContain("Разрешите микрофон");
    expect(harness.socket.closed).toBe(true);
  });

  it("keeps_finishing_when_the_microphone_refuses_after_done", async () => {
    let refuse: (error: unknown) => void = () => undefined;
    const ready = new Promise<void>((_, reject) => {
      refuse = reject;
    });
    const harness = setup({ captureReady: ready });
    const starting = harness.session.start();
    harness.socket.handlers.open();
    harness.session.finish();
    await harness.clock.advanceAsync(0);
    refuse(Object.assign(new Error("denied"), { name: "NotAllowedError" }));
    await starting;
    expect(harness.session.view.phase).toBe("finishing");
    expect(harness.socket.texts.at(-1)).toEqual({ type: "finish" });
  });

  it("hides_the_button_when_the_device_has_no_microphone", async () => {
    const harness = setup({ captureError: Object.assign(new Error("missing"), { name: "NotFoundError" }) });
    await harness.session.start();
    expect(harness.session.view).toMatchObject({ phase: "failed", mic: "missing" });
    expect(harness.session.view.message).toBe("Микрофон не найден");
  });
});

describe("DictationSession slow link", () => {
  it("warns_about_the_link_and_then_finishes_the_recording", async () => {
    const harness = await listening();
    harness.text("Привет", "", 1);
    harness.socket.bufferedAmount = 70_000;
    await harness.clock.advanceAsync(500);
    expect(harness.last()).toMatchObject({ phase: "listening", slow: true, message: "Связь медленная" });
    await harness.clock.advanceAsync(500);
    expect(harness.session.view.phase).toBe("finishing");
    await harness.clock.advanceAsync(0);
    expect(harness.socket.texts.at(-1)).toEqual({ type: "finish" });
  });
});
