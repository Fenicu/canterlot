<script lang="ts">
  import Gauge from '#lib/ui/Gauge.svelte';
  import type { currentBlocks } from './model.js';
  let { blocks }: { blocks: ReturnType<typeof currentBlocks> } = $props();
</script>
<div class="current">
  {#each blocks as block (block.provider)}
    <section class="provider" data-provider={block.provider} aria-label="{block.label}, текущие лимиты">
      <h3>{block.label}{#each block.markers as marker (marker)}<span class="marker" class:reserve={marker === 'резерв недели'}>{marker}</span>{/each}</h3>
      {#if block.empty}<p class="none">нет данных</p>
      {:else}<div class="rows">
        {#each block.windows as row (row.key)}
          <div class="row" data-window={row.key} data-tone={row.tone} data-stale={row.stale || undefined}>
            <span class="label">{row.label}</span>
            <span class="gauge"><Gauge value={row.fill} tone={row.tone} dim={row.stale} label="{block.label}, {row.label}" /></span>
            <b class="value">{row.value}</b>
            <span class="reset">{row.reset}</span>
          </div>
        {/each}
      </div>{/if}
    </section>
  {/each}
</div>
<style>
  .current { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, calc(var(--gauge-width) * 3)), 1fr)); align-items: start; gap: var(--s4); margin-block: var(--s4) var(--s8); }
  .provider { min-width: 0; padding: var(--s6) var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  h3 { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s2) var(--s4); margin: var(--s0) var(--s0) var(--s3); font-size: var(--fs-body); font-weight: var(--weight-semibold); }
  .marker { color: var(--muted); font-size: var(--fs-sm); font-weight: var(--weight-normal); }
  .marker.reserve { color: var(--warning); }
  .rows { display: grid; /* Колонки одной ширины во всех карточках: полоски провайдеров начинаются и кончаются на одной вертикали. */
    grid-template-columns: minmax(calc(var(--s4) * 6), max-content) minmax(var(--s13), 1fr) minmax(calc(var(--s4) * 12), max-content) minmax(calc(var(--s4) * 11), max-content); align-items: center; gap: var(--s3) var(--s4); }
  .row { display: contents; }
  .label, .reset { color: var(--muted); font-size: var(--fs-sm); white-space: nowrap; }
  .gauge { display: flex; min-width: 0; --gauge-width: 100%; }
  .value { font-size: var(--fs-meta); font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .reset { font-variant-numeric: tabular-nums; }
  [data-tone='warning'] .value { color: var(--warning); }
  [data-tone='danger'] .value { color: var(--danger); }
  [data-stale] .value { color: var(--muted); font-weight: var(--weight-normal); }
  .none { margin: var(--s0); color: var(--muted); font-size: var(--fs-meta); }
</style>
