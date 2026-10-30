<script lang="ts">
  import { resolve } from '$app/paths';
  import Icon from '#lib/ui/Icon.svelte';
  import { limitStrip } from './limitsText';
  import type { LimitThresholds, ProviderLimits } from '#lib/stores/limits.js';

  let {
    providers,
    now,
    thresholds,
    prefer = '',
    crunch = false,
    compact = false
  }: {
    providers: ProviderLimits[];
    now: number;
    thresholds: LimitThresholds;
    prefer?: string;
    crunch?: boolean;
    compact?: boolean;
  } = $props();

  const view = $derived(limitStrip(providers, now, thresholds, { prefer, crunch }));
  const shown = $derived(compact ? view.rows.slice(0, 1) : view.rows);
</script>

{#if providers.length > 0}
  <details class="limits" data-tone={view.tone}>
    <summary>
      <span class="rows">
        {#if view.crunchLabel}
          <span>{view.crunchLabel}</span>
        {:else}
          {#each shown as row (row.label)}
            {#if row.empty}
              <span class="row">{compact ? 'Лимиты' : row.label} · нет данных</span>
            {:else}
              <span class="row">{compact ? 'Лимиты ·' : row.label} <span class="pair">5ч <b class:hot={row.hot.fiveHour}>{row.fiveHour}</b></span> · <span class="pair">неделя <b class:hot={row.hot.weekly}>{row.weekly}</b></span></span>
            {/if}
          {/each}
        {/if}
        {#if view.tone === 'reserve'}<span class="reserve">резерв</span>{/if}
      </span>
      <span class="chevron"><Icon name="down" /></span>
    </summary>
    <div class="open">
      <p>{view.details}</p>

      {#if view.staleNote}
        <p>{view.staleNote}</p>
      {/if}

      <a href={resolve('limits')}>История лимитов</a>
    </div>
  </details>
{/if}

<style>
  .limits {
    margin: var(--s0) var(--gutter) var(--s3);
    font-size: var(--fs-sm);
  }
  summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: var(--target);
    padding-block: var(--s2);
    border-radius: var(--radius-pill);
    color: var(--secondary);
    cursor: pointer;
    list-style: none;
  }
  summary::-webkit-details-marker { display: none; }
  summary::marker { content: ''; }
  .rows { display: flex; flex-wrap: wrap; gap: var(--s1) var(--s6); min-width: 0; }
  :global([data-pane='list']) .rows { flex-direction: column; }
  .pair { white-space: nowrap; }
  b { font-weight: var(--weight-semibold); }
  .hot, .reserve { color: var(--warning); }
  .chevron { flex: none; color: var(--secondary); transition: transform var(--duration-fast) var(--ease); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .limits[open] .chevron { transform: rotate(180deg); }
  .open { padding-bottom: var(--s4); }
  p { margin: 0 0 var(--s2); white-space: pre-line; color: var(--secondary); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
</style>
