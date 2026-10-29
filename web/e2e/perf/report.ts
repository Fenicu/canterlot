import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import type { Distribution } from './measure';

const DIR = resolve(process.cwd(), 'e2e/.stack/perf');

/** Каждое распределение лежит отдельным файлом: повтор одного замера обновляет только свою строку отчёта. */
export function saveDistribution(distribution: Distribution): void {
  mkdirSync(DIR, { recursive: true });
  writeFileSync(join(DIR, `${distribution.metric.split(':')[0]!.trim().replace(/[^A-Za-z0-9_-]+/g, '_')}.json`), JSON.stringify(distribution, null, 2));
  process.stdout.write(`${JSON.stringify(distribution)}\n`);
}

export function loadDistributions(): Distribution[] {
  try {
    return readdirSync(DIR)
      .filter((name) => name.endsWith('.json'))
      .sort()
      .map((name) => JSON.parse(readFileSync(join(DIR, name), 'utf8')) as Distribution);
  } catch {
    return [];
  }
}

function detail(d: Distribution): string {
  return `### ${d.metric}

- Цель: ${d.target}; результат: ${d.pass ? 'в пределах цели' : 'вне цели (первый замер, не блокирует)'}.
- Образцов: ${d.n}; длительность прогона ${(d.durationMs / 1000).toFixed(1)} с.
- Мин ${d.min} мс, p50 ${d.p50} мс, p95 ${d.p95} мс, p99 ${d.p99} мс, макс ${d.max} мс.
- Условия:
${Object.entries(d.conditions).map(([key, value]) => `  - \`${key}\`: ${value}`).join('\n')}
`;
}

export function renderReport(distributions: Distribution[]): string {
  const rows = distributions
    .map((d) => `| ${d.metric.split(':')[0]} | ${d.n} | ${d.p50} | ${d.p95} | ${d.p99} | ${d.target} | ${d.pass ? 'в цели' : 'вне цели'} |`)
    .join('\n');
  return `# Первые замеры PWA: P-01…P-06 и дымовые P-13/P-14

Первые числа по ТЗ §16.2 на настоящих хабе и воркере с синтетическим адаптером. Это первые, информационные замеры: промах мимо цели фиксируется здесь и не блокирует этап. Чек-лист с настоящим телефоном и рабочим Firefox: \`docs/perf/phone-acceptance.md\`.

## Как мерили

- Телефонный проект Playwright: Chromium 412×892, касание, дроссель процессора 4× через CDP (\`Emulation.setCPUThrottlingRate\`).
- Сеть: loopback; для P-02 и второго ряда P-06 кромка добавляет по 40 мс в каждую сторону (RTT 80 мс, профиль §16.1).
- Хаб, воркер, браузер и кромка работают на одном хосте, поэтому часы общие: пробы берут сырые \`Date.now()\` и временные метки самого хаба, а \`X-Server-Time-Ms\` в числа не входит.
- Запуск: \`npm run stack:build && npm run test:perf\` из \`web/\`; отчёт пересобирается из файлов \`web/e2e/.stack/perf/*.json\`.

## Честные ограничения

- Эмулированный телефон на настольном процессоре: дроссель не воспроизводит память, термоограничение и планировщик Android.
- Loopback без потерь и без радиоканала; задержка кромки постоянная, джиттера нет.
- Хост общий: рядом идут сборки других задач, поэтому рядом с каждым распределением записана средняя нагрузка. Число образцов меньше итогового профиля §16.1.
- Реальные числа на Galaxy S25 Ultra и трасса \`chrome://inspect\` для P-13 и P-14 получаются по чек-листу.

## Сводка

| Замер | N | p50, мс | p95, мс | p99, мс | Цель | Итог |
| --- | ---: | ---: | ---: | ---: | --- | --- |
${rows}

${distributions.some((d) => !d.pass) ? `Вне цели в этом прогоне: ${distributions.filter((d) => !d.pass).map((d) => d.metric.split(':')[0]).join(', ')}. Это первые числа на эмулированном телефоне при дросселе процессора 4× и общей нагрузке хоста; они не блокируют этап и служат точкой отсчёта.` : 'Все замеры этого прогона уложились в цели.'}

## Подробно

${distributions.map(detail).join('\n')}`;
}

export function writeReport(path = resolve(process.cwd(), '../docs/perf/pwa-first-measurements.md')): void {
  const distributions = loadDistributions();
  if (distributions.length === 0) return;
  mkdirSync(resolve(path, '..'), { recursive: true });
  writeFileSync(path, renderReport(distributions));
}
