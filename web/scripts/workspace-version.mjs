import { readFileSync } from "node:fs";

const cargoPath = new URL("../../Cargo.toml", import.meta.url);

/** Версия из `[workspace.package]`: единственный источник для сборки PWA. */
export function workspaceVersion(text = readFileSync(cargoPath, "utf8")) {
  const section = text.match(/\[workspace\.package\]([\s\S]*?)(?=\n\[|$)/);
  const version = section?.[1]?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error("в Cargo.toml нет [workspace.package] version");
  return version;
}
