<script lang="ts">
  import { styleProperty } from '#lib/ui/style-property.js';
  import type { historyCharts } from './model.js';
  let { charts }: { charts: ReturnType<typeof historyCharts> } = $props();
</script>
<div class="charts">
  {#each charts as chart (chart.provider)}
    <figure class="chart" data-provider={chart.provider} role="img" aria-label="Недельный остаток по дням. {chart.summary}">
      <figcaption aria-hidden="true"><span class="name">{chart.label}</span>{#if chart.today}<span class="today">сегодня <b>{chart.today}</b></span>{/if}</figcaption>
      <div class="plot" aria-hidden="true">
        {#each chart.days as day (day.day)}
          <div class="day" data-missing={day.value === null || undefined}>
            <span class="track"><span class="num">{day.text || '–'}</span><span class="bar" data-series="weekly" use:styleProperty={{ name: '--fill', value: `${(day.value ?? 0) / 100}` }}></span></span>
            <small>{day.mark}</small>
          </div>
        {/each}
      </div>
    </figure>
  {/each}
</div>
<div class="sr-only"><table aria-label="Недельный остаток по дням"><thead><tr><th>День</th><th>Провайдер</th><th>Неделя, осталось</th></tr></thead><tbody>
  {#each charts as chart (chart.provider)}{#each chart.days as day (day.day)}<tr><th>{day.day}</th><td>{chart.label}</td><td>{day.text || 'Нет данных'}</td></tr>{/each}{/each}
</tbody></table></div>
<style>
  .charts { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, calc(var(--gauge-width) * 3)), 1fr)); align-items: start; gap: var(--s4); }
  .chart { min-width: 0; margin: var(--s0); padding: var(--s6) var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  figcaption { display: flex; justify-content: space-between; align-items: baseline; gap: var(--s4); margin-bottom: var(--s4); }
  .name { font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .today { color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .today b { color: var(--text); font-size: var(--fs-meta); font-variant-numeric: tabular-nums; }
  .plot { display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); gap: var(--s3); }
  .day { display: flex; flex-direction: column; align-items: center; gap: var(--s1); min-width: 0; }
  .num { font-size: var(--fs-xs); font-variant-numeric: tabular-nums; white-space: nowrap; color: var(--secondary); }
  [data-missing] .num { color: var(--muted); }
  /* Подпись стоит прямо над столбиком: шкала 0–100% занимает высоту дорожки без строки подписи. */
  .track { display: flex; flex-direction: column; justify-content: end; align-items: center; gap: var(--s1); width: 100%; height: calc(var(--chart-height) * .6); border-bottom: var(--hairline) solid var(--line); }
  .bar { flex: none; display: block; width: 100%; height: calc((100% - var(--s8)) * var(--fill)); background: var(--accent); border-radius: var(--s2) var(--s2) var(--s0) var(--s0); }
  small { font-size: var(--fs-xs); color: var(--muted); }
</style>
