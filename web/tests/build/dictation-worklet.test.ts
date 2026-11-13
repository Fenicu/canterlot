import { expect, test } from "vitest";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const build = join(process.cwd(), "build");
const workers = join(build, "_app/immutable/workers");

function javaScriptFiles(dir: string): string[] {
  return existsSync(dir)
    ? readdirSync(dir)
        .filter((file) => file.endsWith(".js"))
        .map((file) => join(dir, file))
    : [];
}

test("dictation_worklet_is_a_hashed_asset_on_the_same_origin", (ctx) => {
  if (!existsSync(build)) ctx.skip("нет build/: production-сборка не запускалась");
  // Модуль worklet лежит отдельным файлом с хешем: его грузит audioWorklet.addModule.
  const asset = javaScriptFiles(workers).find((file) =>
    readFileSync(file, "utf8").includes("registerProcessor"),
  );
  expect(asset, "нет собранного worklet с registerProcessor").toBeTruthy();
  expect(asset!).toMatch(/worklet-[A-Za-z0-9_-]+\.js$/);
  // Ни blob:, ни data: — CSP производства разрешает только скрипты своего адреса.
  const chunks = [
    ...javaScriptFiles(join(build, "_app/immutable/chunks")),
    ...javaScriptFiles(join(build, "_app/immutable/entry")),
    ...javaScriptFiles(join(build, "_app/immutable/nodes")),
  ];
  const references = chunks.filter((file) =>
    /workers\/worklet-[A-Za-z0-9_-]+\.js/.test(readFileSync(file, "utf8")),
  );
  expect(references.length, "worklet нигде не подключён").toBeGreaterThan(0);
  for (const file of references) {
    const text = readFileSync(file, "utf8");
    expect(text).not.toContain("blob:");
    expect(text).not.toContain("data:text/javascript");
  }
});
