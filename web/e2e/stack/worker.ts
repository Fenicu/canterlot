import { writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

export interface SyntheticLimits {
  five_hour?: { used_permille: number; resets_at_ms: number };
  weekly?: { used_permille: number; resets_at_ms: number };
}

export function writeSyntheticLimits(workerDataDir: string, provider: 'claude' | 'codex', limits: SyntheticLimits): void {
  const dir = join(workerDataDir, 'synthetic-limits');
  mkdirSync(dir, { recursive: true });
  const file = join(dir, `${provider}.json`);
  writeFileSync(file, JSON.stringify(limits, null, 2), 'utf8');
}
