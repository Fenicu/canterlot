<script lang="ts">
  import { styleProperty } from '#lib/ui/style-property.js';
  import { meterTone, type LimitThresholds } from '#lib/stores/limits.js';
  import type { ChartSlot } from './model.js';
  let { charts, thresholds }: { charts: ChartSlot[]; thresholds: LimitThresholds } = $props();

  /** Ширина условной сетки SVG: столбец дня — 100 единиц, высота — 100 процентов остатка. */
  const DAY = 100;

  /** Линия рвётся на дне без снимка: соединять соседей через пропуск значило бы выдумать значение. */
  function segments(days: { value: number | null }[]): string[] {
    const runs: string[][] = [[]];
    days.forEach((day, index) => {
      if (day.value === null) { if (runs.at(-1)!.length) runs.push([]); return; }
      runs.at(-1)!.push(`${index * DAY + DAY / 2},${100 - day.value}`);
    });
    return runs.filter(run => run.length > 1).map(run => run.join(' '));
  }

  /** Пороги из настроек: жёлтый и резерв. Подпись оси прячется, если упирается в более важную. */
  const lines = $derived.by(() => {
    const marks = [
      { key: 'warn', value: thresholds.warn_remaining_pct, label: `${thresholds.warn_remaining_pct}%`, name: `порог ${thresholds.warn_remaining_pct}%` },
      { key: 'reserve', value: thresholds.reserve_remaining_pct, label: `${thresholds.reserve_remaining_pct}%`, name: `резерв ${thresholds.reserve_remaining_pct}%` }
    ].filter(mark => mark.value > 0 && mark.value < 100);
    return marks.filter((mark, index) => marks.findIndex(other => other.value === mark.value) === index);
  });
  const ticks = $derived.by(() => {
    const all = [...lines.map(line => ({ key: line.key, value: line.value, label: line.label })), { key: 'top', value: 100, label: '100%' }, { key: 'mid', value: 50, label: '50%' }, { key: 'zero', value: 0, label: '0%' }];
    // Подписи ближе 12 пунктов друг к другу слипаются: остаётся первая, пороги важнее сетки.
    return all.filter((tick, index) => all.slice(0, index).every(other => Math.abs(other.value - tick.value) >= 12));
  });
  /** Высота подписи и зазор над точкой в процентах поля: 13,75 px и 6 px при поле 104 px. */
  const LABEL_HEIGHT = 13.5;
  const LABEL_LIFT = 5.8;
  /** Низ подписи значения: над точкой, а если там проходит линия порога — над самой линией.
      Порог важнее привычного места: подпись «8%» не должна читаться как зачёркнутая линией 20%. */
  function labelBottom(value: number): number {
    let bottom = value + LABEL_LIFT;
    for (let step = 0; step < lines.length + 1; step++) {
      const crossing = lines.filter((line) => line.value > bottom - 0.5 && line.value < bottom + LABEL_HEIGHT - 0.5);
      if (!crossing.length) break;
      bottom = Math.max(...crossing.map((line) => line.value)) + LABEL_LIFT / 3;
    }
    return bottom;
  }
  /** Точка дня на поле: доля ширины и остаток, в CSS-переменных — разметка без встроенных стилей. */
  function place(node: HTMLElement, at: { x: string; y: string }) {
    const update = (next: typeof at) => { node.style.setProperty('--x', next.x); node.style.setProperty('--y', next.y); };
    update(at);
    return { update };
  }
  const legend = $derived(lines.map(line => line.name).join(', '));
</script>
<div class="charts">
  {#each charts as chart (chart.key)}
    {#if chart.known}
      {@const todayTone = chart.days.at(-1)?.value == null ? 'normal' : meterTone(chart.days.at(-1)!.value!, thresholds)}
      <figure class="chart" data-provider={chart.provider} data-account={chart.named ? chart.account : undefined} role="img" aria-label="Недельный остаток по дням. {chart.summary}{legend ? `; ${legend}` : ''}">
        <figcaption aria-hidden="true"><span class="name">{chart.label}</span>{#if chart.today}<span class="today">сегодня <b data-tone={todayTone}>{chart.today}</b></span>{/if}</figcaption>
        <div class="plot" aria-hidden="true">
          <div class="axis">
            {#each ticks as tick (tick.key)}<span class="tick" data-kind={tick.key} use:styleProperty={{ name: '--y', value: `${tick.value}` }}>{tick.label}</span>{/each}
          </div>
          <div class="area">
            {#each [100, 50, 0] as grid (grid)}<span class="grid" use:styleProperty={{ name: '--y', value: `${grid}` }}></span>{/each}
            {#each lines as line (line.key)}<span class="threshold" data-kind={line.key} use:styleProperty={{ name: '--y', value: `${line.value}` }}></span>{/each}
            <svg viewBox="0 0 {DAY * 7} 100" preserveAspectRatio="none" focusable="false">
              {#each segments(chart.days) as points, index (index)}<polyline {points} vector-effect="non-scaling-stroke" />{/each}
            </svg>
            {#each chart.days as day, index (day.day)}
              {#if day.value !== null}
                {@const x = `${((index + .5) / 7) * 100}%`}
                <span class="dot" data-series="weekly" data-tone={meterTone(day.value, thresholds)} use:place={{ x, y: `${day.value}` }}></span>
                <span class="num" data-tone={meterTone(day.value, thresholds)} use:place={{ x, y: `${labelBottom(day.value)}` }}>{day.text}</span>
              {/if}
            {/each}
          </div>
          <div class="days">
            {#each chart.days as day, index (day.day)}<span class="day" data-missing={day.value === null || undefined} data-today={index === chart.days.length - 1 || undefined}>{day.weekday} {day.mark}</span>{/each}
          </div>
        </div>
      </figure>
    {:else}
      <div class="chart empty" data-provider={chart.provider} data-account={chart.named ? chart.account : undefined}><span class="name">{chart.label}</span><p>Снимков за эти дни нет</p></div>
    {/if}
  {/each}
</div>
<div class="sr-only"><table aria-label="Недельный остаток по дням"><thead><tr><th>День</th><th>Провайдер</th><th>Неделя, осталось</th></tr></thead><tbody>
  {#each charts as chart (chart.key)}{#if chart.known}{#each chart.days as day (day.day)}<tr><th>{day.day}</th><td>{chart.label}</td><td>{day.text || 'Нет данных'}</td></tr>{/each}{/if}{/each}
</tbody></table></div>
<style>
  /* Колонка не уже 224 px: в панели 636 px три графика по 206 px режут дни и наезжают подписями. */
  .charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, calc(var(--gauge-width) * 2)), 1fr)); align-items: stretch; gap: var(--s4); }
  .chart { min-width: 0; margin: var(--s0); padding: var(--s6) var(--s8) var(--s6); background: var(--surface); border-radius: var(--radius-card); }
  figcaption { display: flex; justify-content: space-between; align-items: baseline; gap: var(--s4); }
  .name { min-width: 0; overflow-wrap: anywhere; font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .today { color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .today b { color: var(--text); font-size: var(--fs-meta); font-variant-numeric: tabular-nums; }
  .today b[data-tone='warning'] { color: var(--warning); }
  .today b[data-tone='danger'] { color: var(--danger); }
  /* Ось слева, поле графика, подписи дней под полем; над полем запас под подпись верхней точки. */
  .plot { display: grid; grid-template-columns: max-content minmax(0, 1fr); grid-template-rows: var(--chart-height) auto; column-gap: var(--s3); row-gap: var(--s2); margin-top: var(--s9); font-size: var(--fs-xs); font-variant-numeric: tabular-nums; }
  .axis, .area { position: relative; }
  /* Подписи оси стоят абсолютно, поэтому ширину колонки задаёт сама ось: под «100%». */
  .axis { width: calc(var(--s4) * 4); }
  .tick { position: absolute; right: var(--s0); bottom: calc(var(--y) * 1%); transform: translateY(50%); color: var(--muted); line-height: var(--lh-tight); white-space: nowrap; }
  .tick[data-kind='warn'] { color: var(--warning); }
  .tick[data-kind='reserve'] { color: var(--danger); }
  .grid, .threshold { position: absolute; inset-inline: var(--s0); bottom: calc(var(--y) * 1%); height: var(--s0); border-top: var(--hairline) solid var(--line); }
  .threshold[data-kind='warn'] { border-top-style: dashed; border-top-color: var(--warning); }
  .threshold[data-kind='reserve'] { border-top-color: var(--danger); }
  svg { position: absolute; inset: var(--s0); width: 100%; height: 100%; overflow: visible; }
  polyline { fill: none; stroke: var(--accent); stroke-width: var(--s1); stroke-linejoin: round; stroke-linecap: round; }
  .dot { position: absolute; left: var(--x); bottom: calc(var(--y) * 1%); width: var(--mark); height: var(--mark); transform: translate(-50%, 50%); background: var(--accent); border-radius: var(--radius-round); }
  .dot[data-tone='warning'] { background: var(--warning); }
  .dot[data-tone='danger'] { background: var(--danger); }
  /* Значение читается без наведения: подпись над каждой точкой. Низ подписи считает labelBottom:
     она поднимается над линией порога, а не пересекается с ней. */
  .num { position: absolute; left: var(--x); bottom: calc(var(--y) * 1%); transform: translateX(-50%); color: var(--secondary); line-height: var(--lh-tight); white-space: nowrap; }
  .num[data-tone='warning'] { color: var(--warning); }
  .num[data-tone='danger'] { color: var(--danger); }
  .days { grid-column: 2; display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); }
  .day { text-align: center; color: var(--muted); white-space: nowrap; overflow: hidden; }
  .day[data-today] { color: var(--text); font-weight: var(--weight-semibold); }
  .day[data-missing] { color: var(--muted); }
  .empty { display: flex; flex-direction: column; gap: var(--s2); }
  .empty p { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
</style>
