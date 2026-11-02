import { readFileSync } from 'node:fs';

/** Версия из `[workspace.package]`: та же, что подставляет сборка PWA. */
export const APP_VERSION: string = (() => {
  const text = readFileSync(new URL('../../Cargo.toml', import.meta.url), 'utf8');
  const section = text.match(/\[workspace\.package\]([\s\S]*?)(?=\n\[|$)/);
  const version = section?.[1]?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error('в Cargo.toml нет [workspace.package] version');
  return version;
})();
