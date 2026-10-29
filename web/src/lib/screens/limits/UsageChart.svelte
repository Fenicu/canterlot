<script lang="ts">
  import type { components } from '#lib/api/generated/openapi.js';
  import { chartModel, providerName } from './model.js';
  import { styleProperty } from '#lib/ui/style-property.js';
  let { series, days, zone }: { series: components['schemas']['ProviderSeries'][]; days: string[]; zone: string } = $props();
  const model = $derived(chartModel(series, days, zone));
  const summary = $derived(`Дневные снимки лимитов. ${model.map(p => {
    const range = (values: (number | null)[]) => {
      const known = values.filter((v): v is number => v !== null);
      return known.length ? `от ${Math.min(...known).toLocaleString('ru-RU')} до ${Math.max(...known).toLocaleString('ru-RU')}%` : 'нет данных';
    };
    return `${providerName(p.provider)}: 5 часов ${range(p.days.map(d => d.five))}, неделя ${range(p.days.map(d => d.weekly))}`;
  }).join('. ')}`);
</script>
<div class="chart" role="img" aria-label={summary}>
  <div class="legend" aria-hidden="true">
    {#each model as provider (provider.provider)}<span><i class:codex={provider.provider === 'codex'}></i>{providerName(provider.provider)} · 5ч</span><span><i class="week" class:codex={provider.provider === 'codex'}></i>неделя</span>{/each}
  </div>
  <div class="chart-wrap" aria-hidden="true"><div class="axis"><span>100%</span><span>50%</span><span>0</span></div><div class="plot">
    {#each days as day, index (day)}<div class="day"><div class="bars">
      {#each model as provider (provider.provider)}
        {@const point = provider.days[index]!}
        <span class="bar" class:codex={provider.provider === 'codex'} class:missing={point.five === null} data-series="five" use:styleProperty={{ name: '--fill', value: `${point.five ?? 0}%` }}></span>
        <span class="bar week" class:codex={provider.provider === 'codex'} class:missing={point.weekly === null} data-series="weekly" use:styleProperty={{ name: '--fill', value: `${point.weekly ?? 0}%` }}></span>
      {/each}
    </div><small>{Number(day.slice(-2))}</small></div>{/each}
  </div></div>
</div>
<div class="sr-only"><table aria-label="Дневные снимки лимитов"><thead><tr><th>День</th><th>Провайдер</th><th>5 часов</th><th>Неделя</th></tr></thead><tbody>
  {#each model as provider (provider.provider)}{#each provider.days as day (day.day)}<tr><th>{day.day}</th><td>{providerName(provider.provider)}</td><td>{day.five === null ? 'Нет данных' : `${day.five.toLocaleString('ru-RU')}%`}</td><td>{day.weekly === null ? 'Нет данных' : `${day.weekly.toLocaleString('ru-RU')}%`}</td></tr>{/each}{/each}
</tbody></table></div>
<style>
  .chart { background: var(--surface); padding: var(--s8); border-radius: var(--radius-card); }
  .legend { display: flex; flex-wrap: wrap; gap: var(--s4) var(--s8); font-size: var(--fs-meta); margin-bottom: var(--s10); }
  .legend span { display: inline-flex; align-items: center; gap: var(--s3); }
  i { display: inline-block; width: var(--s6); height: var(--s6); background: var(--accent); border: var(--hairline) solid var(--accent); }
  i.codex { background: var(--chart-secondary); border-color: var(--chart-secondary); }
  .chart-wrap { display: flex; gap: var(--s4); }
  .axis { height: var(--chart-height); display: flex; flex-direction: column; justify-content: space-between; color: var(--muted); font-size: var(--fs-xs); }
  .plot { display: flex; flex: 1; min-width: 0; gap: var(--s4); }
  .day { flex: 1; min-width: 0; text-align: center; }
  .bars { height: var(--chart-height); display: flex; align-items: end; gap: var(--s1); }
  .bar { flex: 1; min-width: 0; height: var(--fill); background: var(--accent); border: var(--hairline) solid var(--accent); border-radius: var(--s2) var(--s2) 0 0; }
  .codex { background: var(--chart-secondary); border-color: var(--chart-secondary); }
  .week, i.week { background: var(--transparent); }
  .missing { visibility: hidden; }
  small { font-size: var(--fs-xs); color: var(--muted); }
</style>
