// Контактный лист: наши экраны рядом с утверждёнными PNG макетов. Для глаз человека, не для CI.
// Запуск: `node scripts/contact-sheet.mjs` (рендерит через Playwright и собирает index.html).
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';

export const MOCKUP_NAMES = [
  '01-sessions', '02-chat', '03-chat-light', '04-info-sheet', '05-subagent-lane', '06-question',
  '07-new-session', '08-quota-paused', '09-machine', '10-limits-history', '11-desktop'
];

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const CONTACT_DIR = join(root, 'e2e/.contact');

/** Каталог макетов лежит вне git и не нужен сборке: без него выводятся только наши снимки. */
export function mockupsDirectory() {
  return process.env.CANTERLOT_MOCKUPS ?? resolve(root, '../.superpowers/design/mockups');
}

function similarity(oursPath, mockupPath) {
  const ours = PNG.sync.read(readFileSync(oursPath));
  const mockup = PNG.sync.read(readFileSync(mockupPath));
  if (ours.width !== mockup.width || ours.height !== mockup.height) {
    return { note: `размеры различаются: ${ours.width}x${ours.height} и ${mockup.width}x${mockup.height}` };
  }
  const diff = pixelmatch(ours.data, mockup.data, null, ours.width, ours.height, { threshold: 0.1 });
  return { ratio: Number(((1 - diff / (ours.width * ours.height)) * 100).toFixed(1)) };
}

export function buildContactSheet({ names = MOCKUP_NAMES, outDir = CONTACT_DIR, mockups = mockupsDirectory() } = {}) {
  mkdirSync(join(outDir, 'mockup'), { recursive: true });
  const rows = names.map((name) => {
    const ours = join(outDir, `${name}.png`);
    const theirs = join(mockups, `${name}.png`);
    const row = { name, ours: existsSync(ours), mockup: false, ratio: null, note: null };
    if (existsSync(theirs)) {
      copyFileSync(theirs, join(outDir, 'mockup', `${name}.png`));
      row.mockup = true;
      if (row.ours) Object.assign(row, similarity(ours, theirs));
    }
    process.stdout.write(`${name}: ${row.ours ? 'снимок есть' : 'снимка нет'}${row.ratio !== null ? `, совпадение пикселей ${row.ratio}% (справочно)` : row.note ? `, ${row.note}` : row.mockup ? '' : ', макета рядом нет'}\n`);
    return row;
  });
  const cells = rows.map((row) => `
    <figure>
      <figcaption>${row.name}${row.ratio !== null ? ` · ${row.ratio}%` : ''}</figcaption>
      <div class="pair">
        ${row.ours ? `<img src="${row.name}.png" alt="Наш экран ${row.name}" />` : '<p>нет снимка</p>'}
        ${row.mockup ? `<img src="mockup/${row.name}.png" alt="Макет ${row.name}" />` : '<p>макета нет</p>'}
      </div>
    </figure>`).join('');
  writeFileSync(join(outDir, 'index.html'), `<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><title>Контактный лист canterlot</title>
<style>body{font:14px system-ui;margin:16px;background:#111;color:#eee}.pair{display:flex;gap:12px;align-items:flex-start}img{max-width:46vw;max-height:80vh;height:auto}figure{margin:0 0 32px}</style></head>
<body><h1>Контактный лист: слева наш экран, справа утверждённый макет</h1>${cells}</body></html>\n`);
  return rows;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const run = spawnSync('npx', ['--no-install', 'playwright', 'test', 'e2e/parity/contact-sheet.spec.ts', '--project=phone'], { cwd: root, stdio: 'inherit' });
  process.exit(run.status ?? 1);
}
