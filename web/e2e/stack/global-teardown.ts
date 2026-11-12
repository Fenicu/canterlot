import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync, unlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const STACK_INFO_FILE = join(tmpdir(), 'canterlot-stack-info.json');
const STACK_STATE_FILE = join(tmpdir(), 'canterlot-stack-state.json');

/** PID принадлежит стенду, только если в его командной строке есть каталог стенда. */
function cmdline(pid: number): string {
  try {
    return readFileSync(`/proc/${pid}/cmdline`, 'utf8').replace(/\0/g, ' ');
  } catch {
    return '';
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** Живые процессы, чей аргумент содержит каталог стенда: хаб, воркер, кромка, супервизоры и дочерние CLI. */
function stackPids(stackDir: string): number[] {
  try {
    const out = execFileSync('pgrep', ['-f', stackDir], { encoding: 'utf8' }).trim();
    return out ? out.split(/\s+/).map(Number).filter((pid) => pid !== process.pid) : [];
  } catch {
    return [];
  }
}

export async function stopStack(): Promise<void> {
  if (!existsSync(STACK_INFO_FILE)) return;
  try {
    const info = JSON.parse(readFileSync(STACK_INFO_FILE, 'utf8'));
    const stackDir: string | undefined = info.stackDir;

    const main = [info.pids?.edge, info.pids?.worker, info.pids?.hub]
      .filter((pid): pid is number => typeof pid === 'number')
      .filter((pid) => stackDir && cmdline(pid).includes(stackDir));
    for (const pid of main) {
      try { process.kill(pid, 'SIGTERM'); } catch { /* уже завершился */ }
    }
    const deadline = Date.now() + 5000;
    while (Date.now() < deadline && main.some(alive)) await new Promise((r) => setTimeout(r, 100));
    for (const pid of main.filter(alive)) {
      try { process.kill(pid, 'SIGKILL'); } catch { /* уже завершился */ }
    }

    // Супервизоры исполнений отсоединяются от воркера; их находим по каталогу стенда в аргументах.
    if (stackDir) {
      for (const pid of stackPids(stackDir)) {
        try { process.kill(pid, 'SIGKILL'); } catch { /* уже завершился */ }
      }
    }

    if (info.dbName && info.pgPort) {
      try {
        execFileSync('psql', ['-h', '127.0.0.1', '-p', String(info.pgPort), '-U', 'canterlot', '-d', 'canterlot', '-c', `DROP DATABASE IF EXISTS ${info.dbName} WITH (FORCE);`], {
          env: { ...process.env, PGPASSWORD: 'canterlot' },
          stdio: 'ignore'
        });
      } catch { /* база уже удалена */ }
    }

    if (stackDir && existsSync(stackDir)) {
      try { rmSync(stackDir, { recursive: true, force: true }); } catch { /* каталог занят, остаётся во временной папке */ }
    }
  } catch { /* повреждённый файл состояния ничего не останавливает */ }

  try { unlinkSync(STACK_INFO_FILE); } catch { /* файла уже нет */ }
  try { unlinkSync(STACK_STATE_FILE); } catch { /* файла уже нет */ }
}

export default async function globalTeardown() {
  await stopStack();
}
