import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SharedTicker } from "#lib/ticker.svelte.js";

function setHidden(hidden: boolean): void {
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => hidden,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("shared ticker", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-28T12:00:00.000Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    delete (document as { hidden?: boolean }).hidden;
  });

  it("runs_one_interval_for_all_subscribers_and_ticks_fast_and_slow_cadences", () => {
    const start = vi.spyOn(window, "setInterval");
    const ticker = new SharedTicker();
    const fast = ticker.retain();
    const slow = ticker.retain();
    expect(start).toHaveBeenCalledTimes(1);

    const second = ticker.second;
    vi.advanceTimersByTime(1000);
    expect(ticker.second).toBe(second + 1);
    expect(ticker.now).toBe(Date.now());

    fast();
    slow();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stops_when_the_document_is_hidden_and_resumes_when_visible", () => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => false,
    });
    const ticker = new SharedTicker();
    const release = ticker.retain();
    expect(vi.getTimerCount()).toBe(1);

    setHidden(true);
    expect(vi.getTimerCount()).toBe(0);
    const second = ticker.second;
    vi.advanceTimersByTime(5000);
    expect(ticker.second).toBe(second);

    setHidden(false);
    expect(vi.getTimerCount()).toBe(1);
    const resumed = ticker.second;
    vi.advanceTimersByTime(1000);
    expect(ticker.second).toBe(resumed + 1);
    release();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does_not_start_in_a_hidden_document", () => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => true,
    });
    const ticker = new SharedTicker();
    const release = ticker.retain();
    expect(vi.getTimerCount()).toBe(0);
    setHidden(false);
    expect(vi.getTimerCount()).toBe(1);
    release();
  });
});
