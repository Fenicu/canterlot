<script lang="ts">
  import Gauge from '#lib/ui/Gauge.svelte';
  import CardShell from './CardShell.svelte';
  import { meterTag, type LimitsData } from './defs/limits';
  import type { CardProps } from './types';

  let { call, data, line, word, env }: CardProps<LimitsData> = $props();
</script>

<!-- Те же полосы и цвета, что у полосы лимитов над списком сессий: число — остаток окна. -->
<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'}>
  {#snippet head()}<span class="title">Лимиты</span>{#if data.rows.length === 0}<span class="none">данных пока нет</span>{/if}{/snippet}
  {#if data.rows.length > 0}
    <div class="rows">
      {#each data.rows as row (row.provider)}
        <div class="row" data-provider={row.provider}>
          <span class="name">{row.label}</span>
          {#if row.empty}
            <span class="none">нет данных</span>
          {:else}
            {#each row.meters as meter (meter.window)}
              <span class="meter" data-window={meter.window}>
                <span class="tag">{meterTag(meter.window)}</span>
                <span class="bar" aria-hidden="true"><Gauge value={meter.remaining} tone={meter.tone} label="{row.label} {meterTag(meter.window)}" /></span>
                <b class="pct" data-tone={meter.tone}><span class="sr-only">осталось&nbsp;</span>{meter.text}</b>
              </span>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</CardShell>

<style>
  .title { flex: none; color: var(--text); }
  .none { color: var(--muted); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); --gauge-width: calc(var(--s4) * 2); }
  .row { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s1) var(--s5); min-width: 0; }
  .name { flex: 0 0 calc(var(--s15) + var(--s6)); min-width: 0; overflow: hidden; color: var(--secondary); text-overflow: ellipsis; white-space: nowrap; }
  .meter { display: inline-flex; align-items: baseline; gap: var(--s2); white-space: nowrap; }
  .tag { color: var(--muted); font-size: var(--fs-xs); }
  .bar { display: inline-flex; align-self: center; }
  .pct { font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; }
  .pct[data-tone='warning'] { color: var(--warning); }
  .pct[data-tone='danger'] { color: var(--danger); }
</style>
