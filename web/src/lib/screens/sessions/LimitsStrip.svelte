<script lang="ts">
  import { onDestroy } from 'svelte';
  import { resolve } from '$app/paths';
  import Icon from '#lib/ui/Icon.svelte';
  import Gauge from '#lib/ui/Gauge.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { LimitsRefresh } from '#lib/stores/limitsRefresh.svelte.js';
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

  const view = $derived(limitStrip(providers, now, thresholds, { prefer }));
  const shown = $derived(compact ? view.rows.slice(0, 1) : view.rows);

  const refresher = new LimitsRefresh();
  // Новое показание приходит событием хаба: оно и закрывает ожидание.
  $effect(() => refresher.observe(view.updatedAt));
  onDestroy(() => refresher.dispose());

  /** Полный текст сброса остаётся в title, а по узкой полосе показываем только время. */
  function resetParts(reset: string): { day: string; clock: string } {
    const body = reset.replace(/^сброс\s+/, '');
    const cut = body.indexOf(' ');
    return cut === -1
      ? { day: '', clock: body }
      : { day: body.slice(0, cut), clock: body.slice(cut + 1) };
  }
</script>

{#snippet meter(tag: string, name: string, m: MeterView, window: 'five' | 'week' | 'month', first: boolean = false)}
  <span class="meter" data-window={window} data-stale={m.stale || undefined} data-known={m.known}>
    <span class="name pname" title={first ? name : undefined}>{first ? name : ''}</span>
    <span class="tag">{tag}</span>
    <span class="bar" aria-hidden="true"><Gauge value={m.fill} tone={m.tone} dim={m.stale || !m.known} label="{name} {tag}" /></span>
    <b class="pct" data-tone={m.tone}><span class="sr-only">осталось </span>{m.text}</b>
    {#if m.reset}
      {@const parts = resetParts(m.reset)}
      <span class="reset" title={m.reset}>
        <span class="reset-prefix">сброс&nbsp;</span>{#if parts.day}<span class="reset-day">{parts.day}&nbsp;</span>{/if}{parts.clock}
      </span>
    {/if}
  </span>
{/snippet}

{#if providers.length > 0}
  <details class="limits" data-tone={view.tone}>
    <summary>
      <span class="rows">
        {#each shown as row, index (row.provider)}
          <span class="row">
            <!-- Стрелка живёт в первой строке: в свёрнутой и раскрытой полосе она стоит на одном месте. -->
            {#if index === 0}<span class="chevron"><Icon name="down" /></span>{/if}
            {#if row.empty}
              <span class="meter empty">
                <span class="name pname">{compact ? 'Лимиты' : row.label}</span>
                <span class="none">нет данных</span>
              </span>
            {:else}
              {#if row.five.absent}
                {@render meter('нед', compact ? 'Лимиты' : row.label, row.week, 'week', true)}
              {:else}
                {@render meter('5ч', compact ? 'Лимиты' : row.label, row.five, 'five', true)}
                {@render meter('нед', row.label, row.week, 'week')}
              {/if}
              {#if row.month}{@render meter('мес', row.label, row.month, 'month')}{/if}
              {#if row.reserve}<span class="reserve" title="Остаток недели в резерве">резерв</span>{/if}
            {/if}
            {#if row.estimated}<span class="estimated" title="Расход выведен из локальных журналов, а не получен от провайдера">оценка</span>{/if}
          </span>
        {/each}
      </span>
    </summary>
    <div class="open">
      <div class="status">
        <span class="updated">{view.updated || 'данных пока нет'}</span>
        <span class="refresh" class:pending={refresher.pending}><IconButton icon="retry" label={refresher.pending ? 'Обновляю лимиты' : 'Обновить лимиты'} inert={refresher.pending} onclick={() => refresher.run(view.updatedAt)} /></span>
        <a href={resolve('limits')}>История лимитов</a>
      </div>
      <p class="note" role="status">{refresher.note}</p>
    </div>
  </details>
{/if}

<style>
  .limits {
    margin: var(--s0) var(--gutter) var(--s3);
    font-size: var(--fs-sm);
    --gauge-width: calc(var(--s4) * 2);
    container-type: inline-size;
  }
  summary {
    display: block;
    min-height: var(--target);
    border-radius: var(--radius-card);
    color: var(--secondary);
    cursor: pointer;
    list-style: none;
  }
  summary::-webkit-details-marker { display: none; }
  summary::marker { content: ''; }
  /* Одна сетка на все состояния: имя, метка окна, полоса, значение, сброс и колонка стрелки. */
  .rows {
    /* max-content, а не размер значка: отступ строки достаётся крайним колонкам как поле, и колонка стрелки растёт на него. */
    --chevron-column: max-content;
    display: grid;
    grid-template-columns: minmax(0, max-content) minmax(calc(var(--s4) * 3), max-content) var(--gauge-width) max-content minmax(0, 1fr) var(--chevron-column);
    column-gap: var(--s2);
    align-items: baseline;
    min-width: 0;
  }
  /* Полоса строки идёт на всю ширину полосы лимитов, текст отступает от её края.
     Отступ есть у каждой строки, поэтому колонки совпадают у строк с фоном и без. */
  .row {
    display: grid;
    grid-template-columns: subgrid;
    grid-column: 1 / -1;
    row-gap: var(--s1);
    align-items: baseline;
    padding: var(--s2) var(--s6);
    border-radius: var(--radius-sm);
  }
  .row:nth-child(even) { background: var(--raised); }
  /* Каждое окно — своя строка общей сетки: колонки не сдвигаются от числа окон. */
  .meter {
    display: grid;
    grid-template-columns: subgrid;
    grid-column: 1 / -2;
    align-items: baseline;
    min-width: 0;
  }
  /* Свёрнутая полоса компактна: одна строка на провайдера, 5ч и нед рядом, месяц скрыт. */
  .limits:not([open]) .rows {
    grid-template-columns: minmax(0, 1fr) minmax(calc(var(--s4) * 2.5), max-content) var(--gauge-width) max-content minmax(calc(var(--s4) * 2.5), max-content) var(--gauge-width) max-content var(--chevron-column);
    column-gap: var(--s1);
  }
  .limits:not([open]) .meter { display: contents; }
  .limits:not([open]) .meter[data-window='month'] { display: none; }
  .limits:not([open]) .name:empty { display: none; }
  .limits:not([open]) .meter > .name { grid-column: 1; }
  .limits:not([open]) .meter[data-window='five'] > .tag { grid-column: 2; }
  .limits:not([open]) .meter[data-window='five'] > .bar { grid-column: 3; }
  .limits:not([open]) .meter[data-window='five'] > .pct { grid-column: 4; }
  /* Значение 5ч («устарело») не должно сливаться с меткой недели. */
  .limits:not([open]) .meter[data-window='week'] > .tag { grid-column: 5; padding-inline-start: var(--s3); }
  .limits:not([open]) .meter[data-window='week'] > .bar { grid-column: 6; }
  .limits:not([open]) .meter[data-window='week'] > .pct { grid-column: 7; }
  .name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* Длинное имя («Antigravity», «OpenCode») не прилипает к метке окна. */
  .limits[open] .pname { padding-inline-end: var(--s2); }
  .tag { color: var(--muted); font-size: var(--fs-xs); white-space: nowrap; }
  .bar { display: inline-flex; }
  .pct { font-weight: var(--weight-semibold); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .pct[data-tone='warning'], .reserve { color: var(--warning); }
  .pct[data-tone='danger'] { color: var(--danger); }
  .meter[data-stale] .pct, .meter[data-known='false'] .pct { color: var(--muted); font-weight: var(--weight-normal); }
  .reset { display: none; padding-inline-start: var(--s2); color: var(--muted); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .limits[open] .reset { display: inline; }
  .none { grid-column: 2 / span 4; justify-self: start; color: var(--muted); white-space: nowrap; }
  .reserve { grid-column: 2 / -2; justify-self: start; }
  .estimated { grid-column: 2 / -2; justify-self: start; color: var(--muted); font-size: var(--fs-xs); }
  /* В узкой полосе сброс укорачивается, но никогда не переносится. */
  @container (max-width: 26em) { .reset-prefix { display: none; } }
  @container (max-width: 16em) { .reset-day { display: none; } }
  .chevron {
    grid-row: 1;
    grid-column: -2 / -1;
    align-self: center;
    display: inline-flex;
    margin-inline-start: var(--s2);
    color: var(--secondary);
    transition: transform var(--duration-fast) var(--ease);
  }
  .chevron :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .limits[open] .chevron { transform: rotate(180deg); }
  .open { padding: var(--s2) var(--s6) var(--s4); }
  .status { display: flex; align-items: center; gap: var(--s4); color: var(--secondary); }
  .status a { margin-inline-start: auto; }
  .refresh.pending :global(svg) { animation: spin var(--duration-stage-spin) var(--ease-stage-spin) infinite; }
  @keyframes spin { to { transform: rotate(-360deg); } }
  .note { margin: var(--s0); min-height: var(--s0); color: var(--secondary); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
</style>
