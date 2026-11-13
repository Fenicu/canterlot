import { describe, expect, it } from "vitest";
import {
  BACKOFF_BASE_MS,
  BACKOFF_MAX_MS,
  STATUS_TTL_MS,
  DictationAvailability,
  statusText,
  type AvailabilityDeps,
  type DictationStatusView,
} from "#lib/dictation/availability.js";

const AVAILABLE: DictationStatusView = {
  available: true,
  reason: null,
  mode: "gpu",
  engine: "warm",
};

function setup(fetch: () => Promise<DictationStatusView>) {
  let now = 1_000;
  let calls = 0;
  const deps: AvailabilityDeps = {
    now: () => now,
    schedule: () => () => undefined,
    fetch: () => {
      calls += 1;
      return fetch();
    },
  };
  const availability = new DictationAvailability(deps);
  return {
    availability,
    get calls() {
      return calls;
    },
    advance(ms: number) {
      now += ms;
    },
  };
}

describe("DictationAvailability", () => {
  it("fetches_at_start_and_reuses_the_answer_for_a_minute", async () => {
    const harness = setup(async () => AVAILABLE);
    await harness.availability.ensure();
    expect(harness.calls).toBe(1);
    expect(harness.availability.status).toEqual(AVAILABLE);
    harness.advance(STATUS_TTL_MS - 1);
    await harness.availability.ensure();
    expect(harness.calls).toBe(1);
    harness.advance(1);
    await harness.availability.ensure();
    expect(harness.calls).toBe(2);
  });

  it("refreshes_for_real_when_forced", async () => {
    const harness = setup(async () => AVAILABLE);
    await harness.availability.ensure();
    await harness.availability.ensure(true);
    expect(harness.calls).toBe(2);
  });

  it("backs_off_after_a_failed_request", async () => {
    const harness = setup(async () => {
      throw new Error("network");
    });
    await harness.availability.ensure();
    expect(harness.calls).toBe(1);
    expect(harness.availability.status).toEqual({
      available: false,
      reason: "no_service",
      mode: null,
      engine: null,
    });
    await harness.availability.ensure();
    expect(harness.calls).toBe(1);
    harness.advance(BACKOFF_BASE_MS);
    await harness.availability.ensure();
    expect(harness.calls).toBe(2);
    await harness.availability.ensure();
    expect(harness.calls).toBe(2);
    harness.advance(BACKOFF_BASE_MS * 2);
    await harness.availability.ensure();
    expect(harness.calls).toBe(3);
  });

  it("caps_the_backoff_and_resets_after_success", async () => {
    let fail = true;
    const harness = setup(async () => {
      if (fail) throw new Error("network");
      return AVAILABLE;
    });
    for (let attempt = 0; attempt < 8; attempt += 1) {
      await harness.availability.ensure(true);
      harness.advance(BACKOFF_MAX_MS);
    }
    expect(harness.availability.failures).toBe(8);
    fail = false;
    await harness.availability.ensure(true);
    expect(harness.availability.failures).toBe(0);
    expect(harness.availability.status).toEqual(AVAILABLE);
    harness.advance(STATUS_TTL_MS);
    await harness.availability.ensure();
    expect(harness.calls).toBe(10);
  });

  it("treats_a_missing_route_as_no_service", async () => {
    const harness = setup(async () => {
      throw Object.assign(new Error("not found"), { status: 404 });
    });
    await harness.availability.ensure();
    expect(harness.availability.status?.available).toBe(false);
    expect(harness.availability.reasonText()).toBe("Служба распознавания не подключена");
  });

  it("retries_at_once_after_a_signed_out_refusal", async () => {
    let signedOut = true;
    const harness = setup(async () => {
      if (signedOut) throw Object.assign(new Error("auth"), { name: "AuthRequired" });
      return AVAILABLE;
    });
    await harness.availability.ensure();
    expect(harness.availability.failures).toBe(1);
    signedOut = false;
    await harness.availability.ensure();
    expect(harness.calls).toBe(2);
    expect(harness.availability.status).toEqual(AVAILABLE);
  });

  it("invalidates_so_the_next_ensure_goes_out_at_once", async () => {
    const harness = setup(async () => AVAILABLE);
    await harness.availability.ensure();
    harness.advance(1_000);
    harness.availability.invalidate();
    await harness.availability.ensure();
    expect(harness.calls).toBe(2);
  });
});

describe("statusText", () => {
  it("names_every_unavailable_reason_in_words", () => {
    expect(statusText({ available: false, reason: "disabled", mode: null, engine: null })).toBe(
      "Диктовка выключена в настройках службы распознавания",
    );
    expect(statusText({ available: false, reason: "no_service", mode: null, engine: null })).toBe(
      "Служба распознавания не подключена",
    );
    expect(statusText({ available: false, reason: "unhealthy", mode: null, engine: null })).toBe(
      "Служба распознавания не отвечает",
    );
    expect(statusText({ available: false, reason: null, mode: null, engine: null })).toBe(
      "Диктовка недоступна",
    );
  });

  it("describes_an_available_engine", () => {
    expect(statusText(AVAILABLE)).toBe("Доступна, движок на видеокарте");
    expect(statusText({ ...AVAILABLE, mode: "cpu" })).toBe("Доступна, медленный режим на процессоре");
    expect(statusText({ ...AVAILABLE, mode: null, engine: "cold" })).toBe(
      "Доступна, движок загрузится при первой записи",
    );
    expect(statusText({ ...AVAILABLE, mode: null, engine: null })).toBe("Доступна");
  });
});
