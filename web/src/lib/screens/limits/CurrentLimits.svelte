<script lang="ts">
  import Gauge from '#lib/ui/Gauge.svelte';
  import { timeTip } from '#lib/ui/time-tip.js';
  import type { currentBlocks } from './model.js';
  let { blocks }: { blocks: ReturnType<typeof currentBlocks> } = $props();
</script>
<div class="current">
  {#each blocks as block (block.provider)}
    <section class="provider" data-provider={block.provider} data-tone={block.lead?.tone} data-stale={block.stale || undefined} aria-label="{block.label}, текущие лимиты">
      <header class="head">
        <h3>{block.label}</h3>
        <!-- Крупно — самое тесное свежее окно: сколько осталось там, где кончится раньше всего. -->
        {#if block.lead}<p class="lead" data-window={block.lead.key}><b>{block.lead.pct}</b><span>{block.lead.caption}</span></p>
        {:else if block.stale}<p class="lead"><b>устарело</b><span>{block.seen}</span></p>{/if}
        {#if block.status || block.markers.length || block.providerStatus}<p class="status">{#if block.status}<span class="word">{block.status}</span>{/if}{#each block.markers as marker (marker)}<span class="marker" class:reserve={marker === 'резерв недели'}>{marker}</span>{/each}{#if block.providerStatus}
          <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- внешняя страница статуса провайдера -->
          <a class="pstatus" data-status-tone={block.providerStatus.tone} href={block.providerStatus.url} target="_blank" rel="noopener noreferrer" title={block.providerStatus.title} aria-label={block.providerStatus.title}>{block.providerStatus.company}: {block.providerStatus.text}</a>{/if}</p>{/if}
      </header>
      {#if block.empty}<p class="none">нет данных</p>
      {:else}<div class="rows">
        {#each block.windows as row (row.key)}
          <div class="row" data-window={row.key} data-tone={row.tone} data-stale={row.stale || undefined}>
            <span class="label">{row.label}</span>
            <b class="value">{row.value}</b>
            {#if row.reset}<time class="reset" datetime={new Date(row.resetAt).toISOString()} aria-label={row.resetLong} use:timeTip={row.resetAt}>{row.reset}</time>{/if}
            <span class="gauge"><Gauge value={row.fill} tone={row.tone} dim={row.stale} label="{block.label}, {row.label}" /></span>
            <!-- Прогноз по средней скорости окна: предупреждение, только если окно кончится раньше сброса. -->
            {#if row.forecast}<p class="forecast" data-forecast={row.forecast.before ? 'before' : 'calm'}><span class="verdict">{row.forecast.text}</span><span class="pace" title="Средняя скорость расхода окна">{row.forecast.pace}</span></p>{/if}
          </div>
        {/each}
      </div>{/if}
    </section>
  {/each}
</div>
<style>
  /* Карточки делят ширину поровну: на телефоне столбиком, на рабочем столе в ряд. Та же сетка у недельных графиков ниже. */
  .current { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--limits-column)), 1fr)); gap: var(--s4); }
  .provider { min-width: 0; container-type: inline-size; padding: var(--s6) var(--s8) var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  .head { display: grid; grid-template-columns: minmax(0, 1fr) max-content; grid-template-areas: 'name lead' 'status lead'; align-items: start; column-gap: var(--s4); margin-bottom: var(--s6); }
  h3 { grid-area: name; margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-semibold); line-height: var(--lh-tight); padding-top: var(--s2); }
  .lead { grid-area: lead; display: flex; flex-direction: column; align-items: end; margin: var(--s0); text-align: end; }
  .lead b { font-size: var(--fs-heading); font-weight: var(--weight-semibold); line-height: var(--lh-tight); font-variant-numeric: tabular-nums; letter-spacing: var(--tracking-tight); }
  .lead span { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
  [data-tone='warning'] .lead b, [data-tone='warning'] .word { color: var(--warning); }
  [data-tone='danger'] .lead b, [data-tone='danger'] .word { color: var(--danger); }
  [data-stale] .lead b { color: var(--muted); font-size: var(--fs-title); font-weight: var(--weight-normal); }
  .status { grid-area: status; display: flex; flex-wrap: wrap; gap: var(--s1) var(--s4); margin: var(--s2) var(--s0) var(--s0); font-size: var(--fs-sm); color: var(--muted); }
  .word { font-weight: var(--weight-semibold); }
  .marker.reserve { color: var(--warning); }
  .pstatus { font-weight: var(--weight-semibold); text-decoration: none; }
  .pstatus[data-status-tone='warning'] { color: var(--warning); }
  .pstatus[data-status-tone='danger'] { color: var(--danger); }
  .pstatus:hover, .pstatus:focus-visible { text-decoration: underline; }
  /* Подпись, остаток и сброс — одна строка над полосой; колонки общие, значения окон стоят друг под другом. */
  .rows { display: grid; grid-template-columns: max-content minmax(0, 1fr) max-content; column-gap: var(--s4); row-gap: var(--s6); }
  .row { display: grid; grid-template-columns: subgrid; grid-column: 1 / -1; align-items: center; row-gap: var(--s3); }
  .label { grid-column: 1; }
  .value { grid-column: 2; }
  .reset { grid-column: 3; justify-self: end; }
  .gauge { grid-column: 1 / -1; grid-row: 2; display: flex; min-width: 0; --gauge-width: 100%; }
  .forecast { grid-column: 1 / -1; grid-row: 3; display: flex; flex-wrap: wrap; justify-content: space-between; gap: var(--s1) var(--s4); margin: var(--s0); font-size: var(--fs-sm); color: var(--muted); }
  .forecast[data-forecast='before'] .verdict { color: var(--warning); font-weight: var(--weight-semibold); }
  .pace { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .label, .reset { color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .value { font-size: var(--fs-meta); font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .reset { font-variant-numeric: tabular-nums; }
  .row[data-tone='warning'] .value { color: var(--warning); }
  .row[data-tone='danger'] .value { color: var(--danger); }
  .row[data-stale] .value { color: var(--muted); font-weight: var(--weight-normal); }
  .none { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
  /* В очень узкой карточке сброс уходит под полосу: иначе он вылезает за край. */
  @container (max-width: 15em) {
    .rows { grid-template-columns: max-content minmax(0, 1fr); }
    .reset { grid-column: 1 / -1; grid-row: 3; }
    .forecast { grid-row: 4; }
  }
</style>
