import { describe, it, expect, vi } from "vitest";
import { sha256 as hashBlob } from "#lib/uploads/hasher.js";
import { productionCsp } from "../../../csp.mjs";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";

describe("uploads/hasher", () => {
  it("hashing_streams_slices_and_never_holds_the_whole_file", async () => {
    // Create a 2.5 MiB mock file
    const totalSize = 2.5 * 1024 * 1024;
    const uint8 = new Uint8Array(totalSize);
    for (let i = 0; i < totalSize; i++) {
      uint8[i] = i % 256;
    }
    const expectedHash = bytesToHex(sha256(uint8));

    const file = new File([uint8], "test-stream.bin", {
      type: "application/octet-stream",
    });

    const sliceSpy = vi.spyOn(Blob.prototype, "slice");

    // Slices should be 1 MiB (1048576)
    const result = await hashBlob(
      file,
      () => undefined,
      new AbortController().signal,
    );

    expect(result).toBe(expectedHash);

    // Ensure slice was called at least 3 times (0..1MB, 1..2MB, 2..2.5MB)
    expect(sliceSpy).toHaveBeenCalledTimes(3);

    expect(sliceSpy).toHaveBeenNthCalledWith(1, 0, 1024 * 1024);
    expect(sliceSpy).toHaveBeenNthCalledWith(2, 1024 * 1024, 2 * 1024 * 1024);
    expect(sliceSpy).toHaveBeenNthCalledWith(
      3,
      2 * 1024 * 1024,
      2.5 * 1024 * 1024,
    );

    sliceSpy.mockRestore();
  });

  it("production_csp_allows_the_hashing_module_worker", () => {
    expect(productionCsp()).toContain("worker-src 'self' blob:");
  });
});
