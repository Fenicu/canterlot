import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { brotliDecompressSync, gunzipSync, zstdDecompressSync } from "node:zlib";
import { expect, test } from "vitest";

const script = resolve(process.cwd(), "scripts/precompress.mjs");

/** Детерминированный шум: сжимать его нечем, поэтому соседи не должны появиться. */
function noise(size: number): Buffer {
  const bytes = Buffer.alloc(size);
  let state = 0x9e3779b9;
  for (let index = 0; index < bytes.length; index++) {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    bytes[index] = state & 0xff;
  }
  return bytes;
}

test("precompress_writes_smaller_siblings_and_round_trips", () => {
  const dir = mkdtempSync(join(tmpdir(), "canterlot-precompress-"));
  try {
    const big = Buffer.from("строка журнала сборки и немного разметки ".repeat(2000));
    const small = Buffer.from("крошечный файл");
    const packed = noise(4096);
    writeFileSync(join(dir, "app.js"), big);
    writeFileSync(join(dir, "tiny.css"), small);
    writeFileSync(join(dir, "random.json"), packed);
    writeFileSync(join(dir, "icon.png"), packed);
    // Сосед, который не меньше оригинала, не должен пережить сборку.
    writeFileSync(join(dir, "random.json.gz"), Buffer.from("старый"));

    execFileSync(process.execPath, [script, dir], { stdio: "pipe" });

    const original = join(dir, "app.js");
    expect(statSync(original).size).toBe(big.length);
    for (const suffix of [".br", ".zst", ".gz"]) {
      const sibling = original + suffix;
      expect(existsSync(sibling), suffix).toBe(true);
      expect(statSync(sibling).size, suffix).toBeLessThan(big.length);
    }
    expect(brotliDecompressSync(readFileSync(original + ".br"))).toEqual(big);
    expect(zstdDecompressSync(readFileSync(original + ".zst"))).toEqual(big);
    expect(gunzipSync(readFileSync(original + ".gz"))).toEqual(big);

    expect(existsSync(join(dir, "tiny.css.br"))).toBe(false);
    expect(existsSync(join(dir, "tiny.css.zst"))).toBe(false);
    expect(existsSync(join(dir, "tiny.css.gz"))).toBe(false);

    for (const suffix of [".br", ".zst", ".gz"]) {
      expect(existsSync(join(dir, "random.json" + suffix)), `random.json${suffix}`).toBe(false);
    }
    expect(existsSync(join(dir, "icon.png.br"))).toBe(false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
