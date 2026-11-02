<script lang="ts">
  import { onDestroy } from 'svelte';
  import { resolve } from '$app/paths';
  import Icon from '#lib/ui/Icon.svelte';
  import Gauge from '#lib/ui/Gauge.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { refreshLimits } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import { limitStrip } from './limitsText';
  import type { LimitThresholds, MeterView, ProviderLimits } from '#lib/stores/limits.js';

  let {
    providers,
    now,
    thresholds,
    prefer = '',
    compact = false
  }: {
    providers: ProviderLimits[];
    now: number;
    thresholds: LimitThresholds;
    prefer?: string;
    compact?: boolean;
  } = $props();

  const WAIT_FOR_READING_MS = 10_000;
  const view = $derived(limitStrip(providers, now, thresholds, { prefer }));
  const shown = $derived(compact ? view.rows.slice(0, 1) : view.rows);

  let pending = $state(false);
  let note = $state('');
  let baseline = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function finish(message: string): void {
    clearTimeout(timer);
    pending = false;
    note = message;
  }

  // Новое показание приходит событием хаба: оно и закрывает ожидание.
  $effect(() => {
    if (pending && view.updatedAt > baseline) finish('');
  });

  async function refresh(): Promise<void> {
    if (pending) return;
    pending = true;
    note = '';
    baseline = view.updatedAt;
    try {
      await refreshLimits();
    } catch (error) {
      finish(failure(error));
      return;
    }
    timer = setTimeout(() => finish('Новых данных пока нет'), WAIT_FOR_READING_MS);
  }

  function failure(error: unknown): string {
    if (error instanceof ApiError && error.code === 'too_many_requests') return 'Недавно уже обновляли, повторите через несколько секунд';
    if (error instanceof ApiError && error.code === 'machine_unavailable') return 'Воркер недоступен';
    return 'Не удалось обновить лимиты';
  }

  onDestroy(() => clearTimeout(timer));
</script>

{#snippet meter(tag: string, name: string, m: MeterView, window: 'five' | 'week' | 'month')}
  <span class="meter" data-window={window} data-stale={m.stale || undefined} data-known={m.known}>
    <span class="tag">{tag}</span>
    <span class="bar" aria-hidden="true"><Gauge value={m.fill} tone={m.tone} dim={m.stale || !m.known} label="{name} {tag}" /></span>
    <b class="pct" data-tone={m.tone}><span class="sr-only">осталось </span>{m.text}</b>
    {#if m.reset}<span class="reset">{m.reset}</span>{/if}
  </span>
{/snippet}

{#if providers.length > 0}
  <details class="limits" data-tone={view.tone}>
    <summary>
      <span class="rows">
        {#each shown as row (row.provider)}
          <span class="row">
            <span class="name">{compact ? 'Лимиты' : row.label}</span>
            <span class="meters">
              {#if row.empty}
                <span class="none">нет данных</span>
              {:else}
                {#if !row.five.absent}{@render meter('5ч', row.label, row.five, 'five')}{:else}<span class="slot" aria-hidden="true"></span>{/if}
                {@render meter('нед', row.label, row.week, 'week')}
                {#if row.month}{@render meter('мес', row.label, row.month, 'month')}{/if}
                {#if row.reserve}<span class="reserve" title="Остаток недели в резерве">резерв</span>{/if}
              {/if}
            </span>
          </span>
        {/each}
      </span>
      <span class="chevron"><Icon name="down" /></span>
    </summary>
    <div class="open">
      <div class="status">
        <span class="updated">{view.updated || 'данных пока нет'}</span>
        <span class="refresh" class:pending><IconButton icon="retry" label={pending ? 'Обновляю лимиты' : 'Обновить лимиты'} inert={pending} onclick={refresh} /></span>
        <a href={resolve('limits')}>История лимитов</a>
      </div>
      <p class="note" role="status">{note}</p>
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
    padding-inline: var(--s6);
    border-radius: var(--radius-card);
    color: var(--secondary);
    cursor: pointer;
    list-style: none;
  }
  summary::-webkit-details-marker { display: none; }
  summary::marker { content: ''; }
  .rows { display: flex; flex: 1; flex-direction: column; gap: var(--s2); min-width: 0; }
  .row { display: grid; grid-template-columns: minmax(calc(var(--s4) * 7), 1fr) max-content max-content; align-items: baseline; gap: var(--s2) var(--s4); min-width: 0; }
  .name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meters { display: contents; }
  .meter { display: inline-flex; align-items: center; gap: var(--s2); width: calc(var(--s4) * 11); white-space: nowrap; --gauge-width: calc(var(--s4) * 2); }
  .meter[data-window='five'], .slot { grid-column: 2; }
  .slot { width: calc(var(--s4) * 11); }
  .meter[data-window='week'] { grid-column: 3; }
  .meter[data-window='month'] { grid-column: 2 / -1; grid-row: 2; justify-self: start; }
  .limits[open] .rows { display: flex; flex-direction: column; gap: var(--s2); }
  .limits[open] .row { display: grid; grid-template-columns: auto minmax(0, 1fr); align-items: start; gap: var(--s2) var(--s4); }
  .limits[open] .meters { display: flex; flex-direction: column; align-items: flex-start; gap: var(--s1); }
  .limits[open] .meter { width: auto; }
  .limits[open] .slot { display: none; }
  .tag { color: var(--muted); font-size: var(--fs-xs); min-width: calc(var(--s4) * 3); }
  .bar { display: inline-flex; }
  .pct { font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; min-width: calc(var(--s4) * 3.5); }
  .pct[data-tone='warning'], .reserve { color: var(--warning); }
  .pct[data-tone='danger'] { color: var(--danger); }
  .meter[data-stale] .pct, .meter[data-known='false'] .pct { color: var(--muted); font-weight: var(--weight-normal); }
  .none { grid-column: 2 / -1; justify-self: start; color: var(--muted); }
  .reserve { grid-column: 2 / -1; grid-row: 3; justify-self: start; }
  .reset { display: none; color: var(--muted); }
  .limits[open] .reset { display: inline; }
  .chevron { flex: none; color: var(--secondary); transition: transform var(--duration-fast) var(--ease); }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .limits[open] .chevron { transform: rotate(180deg); }
  .open { padding-bottom: var(--s4); }
  .status { display: flex; align-items: center; gap: var(--s4); color: var(--secondary); }
  .status a { margin-inline-start: auto; }
  .refresh.pending :global(svg) { animation: spin var(--duration-stage-spin) var(--ease-stage-spin) infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  .note { margin: var(--s0); min-height: var(--s0); color: var(--secondary); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
</style>
