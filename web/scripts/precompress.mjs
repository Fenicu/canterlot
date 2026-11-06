#!/usr/bin/env node
// Заранее сжимает статику для Caddy с `file_server { precompressed zstd br gzip }`:
// рядом с каждым сжимаемым файлом кладёт <name>.br, <name>.zst и <name>.gz.
// Параллелит по воркерам: Brotli 11 и zstd 22 заметно нагружают процессор.
//
//   node scripts/precompress.mjs build
//
// Дочерние файлы детерминированы: в gzip-заголовке mtime нулевой, Brotli получает
// режим текста и подсказку размера, уровни фиксированы. Сосед удаляется, если он
// не меньше оригинала (сжатие уже сжатого или случайного содержимого).
import { readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { availableParallelism } from 'node:os';
import { extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import {
  brotliCompressSync, constants, gzipSync, zstdCompressSync,
} from 'node:zlib';

const MIN_BYTES = 1024;
const ZSTD_MAX_LEVEL = 22;
const EXTENSIONS = new Set([
  '.js', '.mjs', '.css', '.html', '.json', '.svg', '.webmanifest', '.txt', '.xml', '.map',
]);

const suffixes = ['.br', '.zst', '.gz'];

function encodeBrotli(data) {
  return brotliCompressSync(data, {
    params: {
      [constants.BROTLI_PARAM_MODE]: constants.BROTLI_MODE_TEXT,
      [constants.BROTLI_PARAM_QUALITY]: 11,
      [constants.BROTLI_PARAM_SIZE_HINT]: data.length,
    },
  });
}

function encodeZstd(data) {
  return zstdCompressSync(data, {
    params: { [constants.ZSTD_c_compressionLevel]: ZSTD_MAX_LEVEL },
  });
}

function encodeGzip(data) {
  const compressed = gzipSync(data, { level: 9, mtime: 0 });
  // Node и так пишет нулевой mtime; обнуляем явно, чтобы вывод не зависел от версии.
  compressed.writeUInt32LE(0, 4);
  return compressed;
}

const encoders = [
  ['.br', encodeBrotli],
  ['.zst', encodeZstd],
  ['.gz', encodeGzip],
];

/** Сжимает один файл и возвращает, какие соседи получились меньше оригинала. */
export function precompressFile(file) {
  const size = statSync(file).size;
  const kept = { br: false, zst: false, gz: false };
  if (size < MIN_BYTES) {
    for (const suffix of suffixes) rmSync(file + suffix, { force: true });
    return kept;
  }
  const data = readFileSync(file);
  for (const [suffix, encode] of encoders) {
    const compressed = encode(data);
    if (compressed.length < size) {
      writeFileSync(file + suffix, compressed);
      kept[suffix.slice(1)] = true;
    } else {
      rmSync(file + suffix, { force: true });
    }
  }
  return kept;
}

/** Все файлы каталога, которые имеет смысл сжимать: рекурсивно и только с известным расширением. */
export function collect(dir) {
  const found = [];
  const walk = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile() && EXTENSIONS.has(extname(entry.name).toLowerCase())) found.push(path);
    }
  };
  walk(dir);
  return found.sort();
}

function runBatch(files) {
  const result = { br: 0, zst: 0, gz: 0 };
  for (const file of files) {
    const kept = precompressFile(file);
    for (const key of Object.keys(result)) if (kept[key]) result[key]++;
  }
  return result;
}

function runWorker(files) {
  return new Promise((resolvePromise, reject) => {
    const worker = new Worker(new URL(import.meta.url), { workerData: { files } });
    let result = null;
    worker.once('message', (message) => { result = message; });
    worker.once('error', reject);
    worker.once('exit', (code) => {
      if (code === 0 && result) resolvePromise(result);
      else reject(new Error(`воркер предсжатия завершился с кодом ${code}`));
    });
  });
}

/** Сжимает каталог целиком. Возвращает число получившихся соседей и время в миллисекундах. */
export async function precompress(dir) {
  const started = performance.now();
  const files = collect(dir);
  const total = { br: 0, zst: 0, gz: 0 };
  const workers = Math.min(availableParallelism(), 8, files.length);
  if (workers <= 1) {
    Object.assign(total, runBatch(files));
  } else {
    // Крупные куски сжимаются дольше: раскладываем файлы по воркерам от больших к малым,
    // чтобы последний воркер не держал всю сборку.
    const batches = Array.from({ length: workers }, () => []);
    const loads = Array.from({ length: workers }, () => 0);
    const sorted = files
      .map((file) => ({ file, size: statSync(file).size }))
      .sort((a, b) => b.size - a.size);
    for (const { file, size } of sorted) {
      const index = loads.indexOf(Math.min(...loads));
      batches[index].push(file);
      loads[index] += size;
    }
    const results = await Promise.all(batches.filter((batch) => batch.length > 0).map(runWorker));
    for (const result of results) {
      for (const key of Object.keys(total)) total[key] += result[key];
    }
  }
  return { files: files.length, ...total, ms: performance.now() - started };
}

if (!isMainThread) {
  parentPort.postMessage(runBatch(workerData.files));
} else if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const dir = process.argv[2] ?? process.env.PWA_BUILD_DIR ?? 'build';
  precompress(dir).then(({ files, br, zst, gz, ms }) => {
    process.stdout.write(
      `предсжатие ${dir}: ${files} файлов, br ${br}, zst ${zst}, gz ${gz} за ${Math.round(ms)} мс\n`,
    );
  }).catch((error) => {
    process.stderr.write(`предсжатие ${dir} не удалось: ${error.message}\n`);
    process.exitCode = 1;
  });
}
