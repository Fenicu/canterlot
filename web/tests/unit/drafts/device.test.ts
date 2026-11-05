import { describe, it, expect, beforeEach } from "vitest";
import { getDeviceId } from "#lib/drafts/device.js";

describe("drafts/device", () => {
  beforeEach(() => localStorage.clear());

  it("creates_a_random_id_once_and_reuses_it", () => {
    const first = getDeviceId();
    expect(first).toMatch(/^[0-9a-f-]{36}$/);
    expect(getDeviceId()).toBe(first);
    expect(localStorage.getItem("canterlot.device_id")).toBe(first);
  });

  it("uses_the_supplied_storage", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    } as unknown as Storage;
    const id = getDeviceId(storage);
    expect(values.get("canterlot.device_id")).toBe(id);
  });
});
