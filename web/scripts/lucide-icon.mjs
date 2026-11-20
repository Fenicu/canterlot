// Геометрия значка Lucide (ISC) одной строкой `d` для локального реестра `src/lib/ui/icons.ts`.
// Ничего не рисуется от руки: фигуры набора (path, circle, ellipse, rect, line, polyline, polygon)
// механически переводятся в команды пути; любой другой элемент останавливает перевод.
//
// Запуск: распаковать `lucide-static@<LUCIDE_VERSION>` в пустой каталог и вызвать
//   node scripts/lucide-icon.mjs <каталог пакета> brain hammer wifi-off wifi
// Скрипт печатает готовые строки реестра. Версия закреплена: пакет другой версии он не читает.
// Лицензия набора (ISC) записана в docs/licenses/THIRD_PARTY.md, её ведёт scripts/gen-licenses.sh.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const LUCIDE_VERSION = '1.77.0';

const NUMBER = '-?(?:\\d+\\.?\\d*|\\.\\d+)';
const n = (value) => String(Number(Number(value).toFixed(3)));

/** Первая команда подпути в склейке обязана быть абсолютной: относительная `m` считалась бы от конца прошлого подпути. */
function absoluteStart(d) {
  const trimmed = d.trim();
  if (!trimmed.startsWith('m')) return trimmed;
  const head = new RegExp(`^m\\s*(${NUMBER})[\\s,]*(${NUMBER})`).exec(trimmed);
  if (!head) throw new Error(`не разобрать начало пути: ${trimmed}`);
  const rest = trimmed.slice(head[0].length);
  // Пары после `m` — неявные относительные линии.
  const implicit = /^[\s,]*[-.\d]/.test(rest);
  return `M${head[1]} ${head[2]}${implicit ? 'l' : ''}${rest.replace(/^[\s,]+/, '')}`;
}

const attrs = (tag) => Object.fromEntries([...tag.matchAll(/([\w-]+)="([^"]*)"/g)].map((m) => [m[1], m[2]]));

function shape(name, a) {
  switch (name) {
    case 'path':
      return absoluteStart(a.d);
    case 'circle': {
      const [cx, cy, r] = [a.cx, a.cy, a.r].map(Number);
      return `M${n(cx - r)} ${n(cy)}a${n(r)} ${n(r)} 0 1 0 ${n(2 * r)} 0a${n(r)} ${n(r)} 0 1 0 ${n(-2 * r)} 0`;
    }
    case 'ellipse': {
      const [cx, cy, rx, ry] = [a.cx, a.cy, a.rx, a.ry].map(Number);
      return `M${n(cx - rx)} ${n(cy)}a${n(rx)} ${n(ry)} 0 1 0 ${n(2 * rx)} 0a${n(rx)} ${n(ry)} 0 1 0 ${n(-2 * rx)} 0`;
    }
    case 'rect': {
      const [x, y, w, h] = [a.x ?? 0, a.y ?? 0, a.width, a.height].map(Number);
      const rx = Math.min(Number(a.rx ?? a.ry ?? 0), w / 2);
      const ry = Math.min(Number(a.ry ?? a.rx ?? 0), h / 2);
      if (!rx && !ry) return `M${n(x)} ${n(y)}h${n(w)}v${n(h)}h${n(-w)}Z`;
      const arc = (dx, dy) => `a${n(rx)} ${n(ry)} 0 0 1 ${n(dx)} ${n(dy)}`;
      return `M${n(x + rx)} ${n(y)}h${n(w - 2 * rx)}${arc(rx, ry)}v${n(h - 2 * ry)}${arc(-rx, ry)}h${n(2 * rx - w)}${arc(-rx, -ry)}v${n(2 * ry - h)}${arc(rx, -ry)}Z`;
    }
    case 'line':
      return `M${n(a.x1)} ${n(a.y1)}L${n(a.x2)} ${n(a.y2)}`;
    case 'polyline':
    case 'polygon': {
      const points = a.points.trim().split(/[\s,]+/).map(Number);
      const pairs = [];
      for (let i = 0; i < points.length; i += 2) pairs.push(`${n(points[i])} ${n(points[i + 1])}`);
      return `M${pairs.join('L')}${name === 'polygon' ? 'Z' : ''}`;
    }
    default:
      throw new Error(`неизвестная фигура: ${name}`);
  }
}

/** Все фигуры значка одной строкой пути. */
export function lucidePath(svg) {
  const body = svg.replace(/<!--[\s\S]*?-->/g, '').replace(/<svg[\s\S]*?>/, '').replace(/<\/svg>/, '');
  return [...body.matchAll(/<([a-zA-Z][\w-]*)\b([^>]*?)\/?>/g)].map((m) => shape(m[1], attrs(m[2]))).join('');
}

/** `wifi-off` → ключ реестра `wifiOff`. */
export function registryKey(name) {
  return name.replace(/-([a-z0-9])/g, (_, c) => c.toUpperCase());
}

/** Строка для `icons.ts`. */
export function registryLine(name, svg) {
  return `${registryKey(name)}: '${lucidePath(svg)}',`;
}

function main([dir, ...names]) {
  if (!dir || names.length === 0) throw new Error('использование: node scripts/lucide-icon.mjs <каталог пакета lucide-static> <значок>…');
  const version = JSON.parse(readFileSync(join(dir, 'package.json'), 'utf8')).version;
  if (version !== LUCIDE_VERSION) throw new Error(`нужен lucide-static ${LUCIDE_VERSION}, в каталоге ${version}`);
  for (const name of names) process.stdout.write(`${registryLine(name, readFileSync(join(dir, 'icons', `${name}.svg`), 'utf8'))}\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main(process.argv.slice(2));
