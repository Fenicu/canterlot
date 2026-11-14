// Параллельный прогон сквозного набора: каждый шард поднимает свой стенд
// (свой хаб, воркер, кромка и база через CANTERLOT_STACK_SLOT), поэтому общее
// состояние — хаос-прокси, резерв лимитов, машина e2e — между шардами не делится.
// Число шардов берётся из CANTERLOT_STACK_SHARDS, иначе из PW_WORKERS (не больше
// четырёх: каждый стенд держит хаб, воркер, кромку и два браузера).
import { spawn } from 'node:child_process';
import { cpus } from 'node:os';

function shardCount() {
  const explicit = Number(process.env.CANTERLOT_STACK_SHARDS ?? '');
  if (Number.isInteger(explicit) && explicit >= 1 && explicit <= 8) return explicit;
  const workers = process.env.PW_WORKERS ?? '';
  if (/^\d+$/.test(workers)) return Math.min(4, Math.max(1, Number(workers)));
  const pct = /^(\d+)%$/.exec(workers);
  if (pct) return Math.min(4, Math.max(1, Math.round((cpus().length * Number(pct[1])) / 100)));
  return Math.min(4, Math.max(2, Math.round(cpus().length / 9)));
}

const shards = shardCount();
const extraArgs = process.argv.slice(2);
process.stdout.write(`stack: ${shards} шарда, свой стенд на шард (CANTERLOT_STACK_SHARDS меняет число)\n`);

const results = await Promise.all(
  Array.from({ length: shards }, (_, i) => {
    const slot = String(i + 1);
    return new Promise((resolve) => {
      const child = spawn(
        'npx',
        [
          '--no-install',
          'playwright',
          'test',
          'e2e/stack',
          `--shard=${i + 1}/${shards}`,
          `--output=test-results/stack-slot-${slot}`,
          ...extraArgs
        ],
        {
          env: { ...process.env, CANTERLOT_STACK_SLOT: slot },
          stdio: ['ignore', 'pipe', 'pipe']
        }
      );
      const tag = (chunk) => {
        for (const line of chunk.toString().split('\n')) {
          if (line.trim()) process.stdout.write(`[shard ${slot}] ${line}\n`);
        }
      };
      child.stdout.on('data', tag);
      child.stderr.on('data', tag);
      child.on('close', (code) => resolve({ slot, code: code ?? 1 }));
    });
  })
);

const failed = results.filter((r) => r.code !== 0);
process.stdout.write(`stack: шардов прошло ${shards - failed.length}/${shards}\n`);
if (failed.length > 0) {
  for (const r of failed) process.stderr.write(`[shard ${r.slot}] exit ${r.code}\n`);
  process.exit(1);
}
