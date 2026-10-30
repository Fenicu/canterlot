import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WorkdirSuggester } from "#lib/screens/sessions/workdirSuggest.js";

type Load = (
  machineId: string,
  path: string,
  signal: AbortSignal,
) => Promise<string[]>;

function setup(load: Load) {
  const shown: string[][] = [];
  const spy = vi.fn(load);
  const suggester = new WorkdirSuggester(spy, (items) => shown.push(items));
  return { suggester, load: spy, shown, last: () => shown.at(-1) };
}

function deferred() {
  let resolve!: (value: string[]) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<string[]>((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe("sessions/workdir suggestions", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("suggestions_wait_for_a_pause_in_typing", async () => {
    const { suggester, load, last } = setup(async () => ["/srv/canterlot"]);
    suggester.input("m1", "/srv/c");
    suggester.input("m1", "/srv/ca");
    await vi.advanceTimersByTimeAsync(199);
    suggester.input("m1", "/srv/can");
    await vi.advanceTimersByTimeAsync(199);
    expect(load).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(load).toHaveBeenCalledTimes(1);
    expect(load.mock.calls[0]?.slice(0, 2)).toEqual(["m1", "/srv/can"]);
    expect(last()).toEqual(["/srv/canterlot"]);
  });

  it("suggestions_ignore_a_late_answer_for_an_older_input", async () => {
    const slow = deferred();
    const { suggester, load, last } = setup(async (_id, path) =>
      path === "/srv/a" ? slow.promise : ["/srv/ab"],
    );
    suggester.input("m1", "/srv/a");
    await vi.advanceTimersByTimeAsync(200);
    expect(load.mock.calls[0]?.[2].aborted).toBe(false);
    suggester.input("m1", "/srv/ab");
    expect(load.mock.calls[0]?.[2].aborted).toBe(true);
    await vi.advanceTimersByTimeAsync(200);
    expect(last()).toEqual(["/srv/ab"]);
    slow.resolve(["/srv/a-old"]);
    await vi.advanceTimersByTimeAsync(0);
    expect(last()).toEqual(["/srv/ab"]);
  });

  it("suggestions_show_nothing_when_the_machine_does_not_answer", async () => {
    const failed = deferred();
    const { suggester, last } = setup(() => failed.promise);
    suggester.input("m1", "/srv/");
    await vi.advanceTimersByTimeAsync(200);
    failed.reject(new Error("timeout"));
    await vi.advanceTimersByTimeAsync(0);
    expect(last()).toEqual([]);
  });

  it("suggestions_do_not_ask_about_a_path_the_form_would_refuse", async () => {
    const { suggester, load } = setup(async () => ["/srv/x"]);
    for (const path of ["", "relative", "/srv/../etc", "  "]) {
      suggester.input("m1", path);
    }
    suggester.input("", "/srv/");
    await vi.advanceTimersByTimeAsync(1000);
    expect(load).not.toHaveBeenCalled();
  });

  it("suggestions_drop_entries_that_no_longer_match_while_waiting", async () => {
    const next = deferred();
    const { suggester, last, load } = setup(async () => [
      "/srv/alpha",
      "/srv/beta",
    ]);
    suggester.input("m1", "/srv/");
    await vi.advanceTimersByTimeAsync(200);
    expect(last()).toEqual(["/srv/alpha", "/srv/beta"]);
    load.mockImplementationOnce(() => next.promise);
    suggester.input("m1", "/srv/al");
    expect(last()).toEqual(["/srv/alpha"]);
  });

  it("suggestions_clear_cancels_what_is_on_the_way", async () => {
    const { suggester, load, last } = setup(async () => ["/srv/x"]);
    suggester.input("m1", "/srv/");
    suggester.clear();
    await vi.advanceTimersByTimeAsync(1000);
    expect(load).not.toHaveBeenCalled();
    expect(last()).toEqual([]);
  });

  it("suggestions_after_a_choice_are_requested_at_once", async () => {
    const { suggester, load } = setup(async () => ["/srv/x/inner"]);
    suggester.input("m1", "/srv/x/", 0);
    await vi.advanceTimersByTimeAsync(0);
    expect(load).toHaveBeenCalledTimes(1);
  });
});
