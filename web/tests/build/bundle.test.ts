import { expect, test } from 'vitest';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, normalize, relative } from 'node:path';
import { gzipSync } from 'node:zlib';

const build = join(process.cwd(), 'build');
const immutable = join(build, '_app/immutable');
const KIB = 1024;

function entryFile(prefix: string): string {
  const name = readdirSync(join(immutable, 'entry')).find((file) => file.startsWith(`${prefix}.`) && file.endsWith('.js'));
  if (!name) throw new Error(`нет файла entry/${prefix}`);
  return join(immutable, 'entry', name);
}

/** Статические зависимости модуля: `import … from`, `import "…"` и `export … from`, без динамических `import(…)`. */
function staticImports(file: string): string[] {
  const text = readFileSync(file, 'utf8');
  const found = new Set<string>();
  for (const match of text.matchAll(/(?:^|[;}\s])(?:import|export)\s*(?:[^'"`();]*?\s*from\s*)?["'`](\.[^"'`]+\.js)["'`]/g)) found.add(normalize(join(dirname(file), match[1]!)));
  return [...found];
}

function closure(files: string[]): Set<string> {
  const seen = new Set<string>();
  const queue = [...files];
  while (queue.length > 0) {
    const file = queue.pop()!;
    if (seen.has(file) || !existsSync(file)) continue;
    seen.add(file);
    queue.push(...staticImports(file));
  }
  return seen;
}

/** Файлы, которые приложение запрашивает при заходе на маршрут: подсказки предзагрузки самого SvelteKit для узлов layout и страницы. */
function routeFiles(route: string): string[] {
  const app = readFileSync(entryFile('app'), 'utf8');
  const list = JSON.parse(/m\.f=(\[[^\]]*\])/.exec(app)![1]!) as string[];
  const nodes = new Map<number, number[]>();
  for (const match of app.matchAll(/import\(`\.\.\/nodes\/(\d+)\.[^`]+\.js`\),__vite__mapDeps\(\[([\d,]*)\]\)/g)) nodes.set(Number(match[1]), match[2]!.split(',').filter(Boolean).map(Number));
  const dictionary = JSON.parse(/i=(\{"\/":\[[^;]*?\}),a=\{/.exec(app)![1]!) as Record<string, number[]>;
  const wanted = [0, ...(dictionary[route] ?? [])];
  const files = new Set<string>();
  for (const node of wanted) for (const index of nodes.get(node) ?? []) files.add(normalize(join(immutable, 'entry', list[index]!)));
  for (const node of wanted) {
    const name = readdirSync(join(immutable, 'nodes')).find((file) => file.startsWith(`${node}.`));
    if (name) files.add(join(immutable, 'nodes', name));
  }
  return [...files].filter((file) => file.endsWith('.js'));
}

function initialJavaScript(route: string): string[] {
  const shell = readFileSync(join(build, '200.html'), 'utf8');
  const preload = [...shell.matchAll(/href="(\/_app\/immutable\/[^"]+\.js)"/g)].map((match) => join(build, match[1]!));
  return [...closure([entryFile('start'), entryFile('app'), ...preload, ...routeFiles(route)])].filter((file) => file.endsWith('.js'));
}

const gzipKiB = (files: string[]): number => files.reduce((sum, file) => sum + gzipSync(readFileSync(file)).length, 0) / KIB;
const all = (dir: string): string[] => (existsSync(dir) ? readdirSync(dir).filter((file) => file.endsWith('.js')).map((file) => join(dir, file)) : []);

test('bundle_budget_holds', async (ctx) => {
  if (!existsSync(build)) ctx.skip('нет build/: production-сборка не запускалась');
  const entry = initialJavaScript('/sessions');
  const size = gzipKiB(entry);
  process.stdout.write(`entry route /sessions: ${entry.length} files, ${size.toFixed(1)} KiB gzip; chat route: ${gzipKiB(initialJavaScript('/c/[id]')).toFixed(1)} KiB gzip\n`);
  expect(entry.length).toBeGreaterThan(3);
  expect(size, `JS входного маршрута ${size.toFixed(1)} КиБ gzip`).toBeLessThanOrEqual(180);

  // Тяжёлые части лежат вне входного маршрута и существуют отдельными файлами.
  const initial = new Set(entry);
  const everything = [...all(join(immutable, 'chunks')), ...all(join(immutable, 'nodes')), ...all(join(immutable, 'workers')), ...all(join(immutable, 'assets'))];
  const lazy = everything.filter((file) => !initial.has(file));
  const where = (signature: RegExp): string[] => everything.filter((file) => signature.test(readFileSync(file, 'utf8')));
  const parts: Record<string, RegExp> = {
    markdown: /linkify/,
    dompurify: /DOMPurify/,
    'html-to-image': /foreignObject/
  };
  for (const [name, signature] of Object.entries(parts)) {
    const holders = where(signature);
    expect(holders.length, `${name}: нужен хотя бы один файл`).toBeGreaterThan(0);
    expect(holders.filter((file) => initial.has(file)).map((file) => relative(build, file)), `${name} не попадает во вход`).toEqual([]);
    expect(holders.every((file) => lazy.includes(file))).toBe(true);
  }
  // Подсветка кода: ядро и языки не должны попасть во вход (сейчас модуль ещё не подключён к рендеру, поэтому в сборке их нет совсем).
  expect(where(/registerLanguage/).filter((file) => initial.has(file))).toEqual([]);
  const workers = all(join(immutable, 'workers'));
  expect(workers.some((file) => /hash\.worker/.test(file)), 'хеш-воркер отдельным файлом').toBe(true);
  expect(entry.some((file) => /hash\.worker/.test(file))).toBe(false);
});
