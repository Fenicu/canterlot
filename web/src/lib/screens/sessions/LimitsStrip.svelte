<script lang="ts">
  import { onDestroy } from 'svelte';
  import { resolve } from '$app/paths';
  import Icon from '#lib/ui/Icon.svelte';
  import Gauge from '#lib/ui/Gauge.svelte';
  import IconButton from '#lib/ui/IconButton.svelte';
  import { LimitsRefresh } from '#lib/stores/limitsRefresh.svelte.js';
  import { limitPlaque, limitStrip } from './limitsText';
  import { fmt } from '#lib/format/ru.js';
  import { resetPhrase } from '#lib/format/countdown.js';
  import type { LimitThresholds, MeterView, ProviderLimits, StatusBadge } from '#lib/stores/limits.js';

  let {
    providers,
    now,
    thresholds,
    prefer = '',
    compact = false,
    plaques = false
  }: {
    providers: ProviderLimits[];
    now: number;
    thresholds: LimitThresholds;
    prefer?: string;
    compact?: boolean;
    /** Свёрнутая полоса — плашки провайдеров в один ряд: окно с меньшим остатком и его полоска. */
    plaques?: boolean;
  } = $props();
  let open = $state(false);

  const view = $derived(limitStrip(providers, now, thresholds, { prefer }));
  const shown = $derived(compact ? view.rows.slice(0, 1) : view.rows);
  const troubled = $derived(view.rows.filter((row) => row.status));

  const refresher = new LimitsRefresh();
  // Новое показание приходит событием хаба: оно и закрывает ожидание.
  $effect(() => refresher.observe(view.updatedAt));
  onDestroy(() => refresher.dispose());

  /** Старшая единица сжатого отсчёта («23ч» из «23ч59м») остаётся и в самой узкой полосе. */
  const SHORT_SPLIT = /^(\D*\d+\D+)(.*)$/;
  function shortHead(text: string): string {
    return SHORT_SPLIT.exec(text)?.[1] ?? text;
  }
  function shortTail(text: string): string {
    return SHORT_SPLIT.exec(text)?.[2] ?? '';
  }

  /** Подсказка над отсчётом: полная дата сброса и, если окно кончится раньше, прогноз. */
  function resetTitle(m: MeterView): string {
    const date = `сброс ${fmt.fullStamp(m.resetAt)}`;
    return m.forecast?.before ? `${date}; ${m.forecast.long}, раньше сброса` : date;
  }
</script>

{#snippet meter(tag: string, name: string, m: MeterView, window: 'five' | 'week' | 'fable' | 'month', first: boolean = false)}
  <span class="meter" data-window={window} data-stale={m.stale || undefined} data-known={m.known}>
    <span class="name pname" title={first ? name : undefined}>{first ? name : ''}</span>
    <span class="tag">{tag}</span>
    <span class="bar" aria-hidden="true"><Gauge value={m.fill} tone={m.tone} dim={m.stale || !m.known} label="{name} {tag}" /></span>
    <b class="pct" data-tone={m.tone}><span class="sr-only">осталось </span>{m.text}</b>
    {#if m.reset}
      <!-- Свёрнутая полоса показывает сжатый отсчёт, раскрытая — словами; полная дата в подсказке. -->
      <span class="reset" data-forecast={m.forecast?.before ? 'before' : undefined} title={resetTitle(m)}>
        <span class="sr-only">{resetPhrase(m.resetLong)}{m.forecast?.before ? `, ${m.forecast.long}` : ''}</span>
        <span class="reset-short" aria-hidden="true">{shortHead(m.resetShort)}{#if shortTail(m.resetShort)}<span class="reset-tail">{shortTail(m.resetShort)}</span>{/if}</span>
        <span class="reset-full" aria-hidden="true">{resetPhrase(m.reset)}</span>
        <span class="reset-bare" aria-hidden="true">{m.reset}</span>
        <span class="reset-span" aria-hidden="true">{m.resetSpan}</span>
      </span>
    {/if}
    {#if m.reset && m.forecast?.before}<span class="forecast" aria-hidden="true">{m.forecast.text}</span>{/if}
  </span>
{/snippet}

<!-- В свёрнутой полосе статус — просто слово: нажатие на него раскрывает полосу, как и всё остальное. -->
{#snippet statusWord(status: StatusBadge)}
  <span class="pstatus" data-status-tone={status.tone}>{status.text}</span>
{/snippet}

<!-- Раскрытая полоса: статус ведёт на страницу провайдера в новой вкладке. -->
{#snippet statusLink(status: StatusBadge, named: boolean)}
  <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- внешняя страница статуса провайдера -->
  <a class="pstatus" data-status-tone={status.tone} href={status.url} target="_blank" rel="noopener noreferrer" title={status.title} aria-label={status.title}>{named ? `${status.company}: ${status.text}` : status.text}</a>
{/snippet}

{#if providers.length > 0}
  <details class="limits" class:plaqued={plaques} data-tone={view.tone} bind:open>
    <summary>
      {#if plaques && !open}
        <span class="plaques" data-limit-plaques>
          {#each view.rows as row (row.provider)}
            {@const plaque = limitPlaque(row)}
            <span class="plaque" data-provider={row.provider}>
              <span class="plaque-text">
                <span class="pname">{row.label}</span>
                {#if row.status}{@render statusWord(row.status)}{/if}
                {#if plaque}
                  <b class="pct" data-tone={plaque.meter.tone} class:dim={plaque.meter.stale || !plaque.meter.known}><span class="sr-only">осталось </span>{plaque.meter.text}</b>
                  <span class="tag">{plaque.tag}</span>
                  {#if plaque.meter.reset}
                    <span class="plaque-reset" data-forecast={plaque.meter.forecast?.before ? 'before' : undefined} title={resetTitle(plaque.meter)}>
                      <span class="sr-only">, {resetPhrase(plaque.meter.resetLong)}{plaque.meter.forecast?.before ? `, ${plaque.meter.forecast.long}` : ''}</span>
                      <span class="reset-short" aria-hidden="true">{plaque.meter.resetShort}</span>
                    </span>
                  {/if}
                {:else}
                  <span class="none">нет данных</span>
                {/if}
                {#if row.reserve}<span class="reserve" title="Остаток недели в резерве">резерв</span>{/if}
              </span>
              {#if plaque}<span class="bar" aria-hidden="true"><Gauge value={plaque.meter.fill} tone={plaque.meter.tone} dim={plaque.meter.stale || !plaque.meter.known} label="{row.label} {plaque.tag}" /></span>{/if}
            </span>
          {/each}
          <span class="sr-only">Подробнее о лимитах</span>
        </span>
      {:else}
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
              {#if row.fable}{@render meter('Fable', row.label, row.fable, 'fable')}{/if}
              {#if row.month}{@render meter('мес', row.label, row.month, 'month')}{/if}
              {#if row.reserve}<span class="reserve" title="Остаток недели в резерве">резерв</span>{/if}
            {/if}
            {#if row.estimated}<span class="estimated" title="Расход выведен из локальных журналов, а не получен от провайдера">оценка</span>{/if}
            {#if row.status}<span class="status-line">{#if open}{@render statusLink(row.status, true)}{:else}{@render statusWord(row.status)}{/if}</span>{/if}
          </span>
        {/each}
      </span>
      {/if}
    </summary>
    <div class="open">
      <div class="status">
        <span class="updated">{view.updated || 'данных пока нет'}</span>
        <span class="refresh" class:pending={refresher.pending}><IconButton icon="retry" label={refresher.pending ? 'Обновляю лимиты' : 'Обновить лимиты'} inert={refresher.pending} onclick={() => refresher.run(view.updatedAt)} /></span>
        <a href={resolve('limits')}>История лимитов</a>
      </div>
      <p class="note" role="status">{refresher.note}</p>
      {#each troubled as row (row.provider)}
        {#if row.status}
          <div class="status-detail" data-provider={row.provider}>
            {@render statusLink(row.status, true)}
            {#if row.status.description}<span class="status-description">{row.status.description}</span>{/if}
            {#each row.status.incidents as incident, index (index)}
              {#if incident.url}
                <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- внешняя страница инцидента -->
                <a class="incident" href={incident.url} target="_blank" rel="noopener noreferrer">{incident.name}</a>
              {:else}
                <span class="incident">{incident.name}</span>
              {/if}
            {/each}
          </div>
        {/if}
      {/each}
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
  /* Свёрнутая полоса компактна: одна строка на провайдера, 5ч и нед рядом, месяц скрыт.
     У каждого окна свой сжатый отсчёт до сброса: колонки 5 и 9. */
  .limits:not([open]) .rows {
    grid-template-columns: minmax(0, 1fr) minmax(calc(var(--s4) * 2.5), max-content) var(--gauge-width) max-content max-content minmax(calc(var(--s4) * 2.5), max-content) var(--gauge-width) max-content max-content var(--chevron-column);
    column-gap: var(--s1);
  }
  .limits:not([open]) .meter { display: contents; }
  .limits:not([open]) .meter[data-window='fable'], .limits:not([open]) .meter[data-window='month'] { display: none; }
  .limits:not([open]) .name:empty { display: none; }
  .limits:not([open]) .meter > .name { grid-column: 1; }
  .limits:not([open]) .meter[data-window='five'] > .tag { grid-column: 2; }
  .limits:not([open]) .meter[data-window='five'] > .bar { grid-column: 3; }
  .limits:not([open]) .meter[data-window='five'] > .pct { grid-column: 4; }
  .limits:not([open]) .meter[data-window='five'] > .reset { grid-column: 5; }
  /* Значение 5ч («устарело») не должно сливаться с меткой недели. */
  .limits:not([open]) .meter[data-window='week'] > .tag { grid-column: 6; padding-inline-start: var(--s3); }
  .limits:not([open]) .meter[data-window='week'] > .bar { grid-column: 7; }
  .limits:not([open]) .meter[data-window='week'] > .pct { grid-column: 8; }
  .limits:not([open]) .meter[data-window='week'] > .reset { grid-column: 9; }
  .limits:not([open]) .reset { display: inline-block; padding-inline-start: var(--s1); font-size: var(--fs-xs); }
  /* У окна без отсчёта («устарело», «нет данных») значение занимает и колонку сброса: длинное слово не раздувает колонку процентов. */
  .limits:not([open]) .meter[data-window='five']:not(:has(.reset)) > .pct { grid-column: 4 / span 2; }
  .limits:not([open]) .meter[data-window='week']:not(:has(.reset)) > .pct { grid-column: 8 / span 2; }
  .limits:not([open]) .reset-full, .limits:not([open]) .reset-bare, .limits:not([open]) .reset-span, .limits[open] .reset-short, .limits[open] .reset-bare, .limits[open] .reset-span { display: none; }
  /* Окно кончится раньше сброса: в свёрнутой полосе места под слова нет, отсчёт окрашен, прогноз в подсказке и раскрытой полосе. */
  .limits:not([open]) .reset[data-forecast='before'] { color: var(--warning); }
  .limits:not([open]) .forecast { display: none; }
  /* Узкая полоса отдаёт место отсчёту сброса: полоски повторяют проценты и уходят первыми,
     затем отсчёт оставляет старшую единицу («23ч» вместо «23ч59м»), точный остаток — в раскрытой полосе. */
  @container (max-width: 25em) {
    .limits:not([open]) .rows { --gauge-width: var(--s0); }
    .limits:not([open]) .bar { display: none; }
  }
  @container (max-width: 19em) {
    .limits:not([open]) .reset-tail { display: none; }
  }
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
  /* Прогноз — своя строка под окном: в строку окна он не помещается на телефоне. */
  .forecast { grid-column: 2 / -1; color: var(--warning); white-space: nowrap; }
  .none { grid-column: 2 / span 4; justify-self: start; color: var(--muted); white-space: nowrap; }
  .reserve { grid-column: 2 / -2; justify-self: start; }
  .estimated { grid-column: 2 / -2; justify-self: start; color: var(--muted); font-size: var(--fs-xs); }
  .status-line { grid-column: 2 / -2; justify-self: start; min-width: 0; }
  /* Слово статуса: цвет по тяжести, без значков. Ссылка не растягивает строку до цели касания. */
  .pstatus { display: inline; min-height: 0; font-weight: var(--weight-semibold); white-space: nowrap; text-decoration: none; }
  .pstatus[data-status-tone='warning'] { color: var(--warning); }
  .pstatus[data-status-tone='danger'] { color: var(--danger); }
  a.pstatus:hover, a.pstatus:focus-visible { text-decoration: underline; }
  .status-detail { display: flex; flex-direction: column; gap: var(--s1); margin-block-start: var(--s2); color: var(--secondary); }
  .status-detail .pstatus { align-self: flex-start; }
  .status-detail .incident { display: block; min-height: 0; color: var(--secondary); overflow-wrap: anywhere; }
  .status-detail a.incident { color: var(--accent-link); }
  /* В узкой раскрытой полосе сброс теряет слово «сброс», затем «через», но никогда не переносится. */
  @container (max-width: 30em) {
    .limits[open] .reset-full { display: none; }
    .limits[open] .reset-bare { display: inline; }
  }
  @container (max-width: 26em) {
    .limits[open] .reset-bare { display: none; }
    .limits[open] .reset-span { display: inline; }
  }
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
  /* Плашки провайдеров в один ряд: имя, остаток, окно и полоска на всю ширину плашки. */
  .plaques { display: flex; gap: var(--s3); min-width: 0; }
  .plaque { flex: 1 1 0; min-width: 0; display: flex; flex-direction: column; justify-content: center; gap: var(--s2); padding: var(--s2) var(--s3) var(--s3); border-radius: var(--radius-sm); background: var(--surface); }
  .plaque-text { display: flex; align-items: baseline; flex-wrap: wrap; column-gap: var(--s2); min-width: 0; overflow: hidden; color: var(--muted); line-height: var(--lh-tight); }
  .plaque .pname { min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  .plaque .reset-short { font-size: var(--fs-xs); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .plaque-reset[data-forecast='before'] { color: var(--warning); }
  .plaque .pct { color: var(--text); }
  .plaque .pct.dim { color: var(--muted); font-weight: var(--weight-normal); }
  .plaque .bar { display: flex; --gauge-width: 100%; }
  .plaques { min-height: var(--target); }
  .status { display: flex; align-items: center; gap: var(--s4); color: var(--secondary); }
  .status a { margin-inline-start: auto; }
  .refresh.pending :global(svg) { animation: spin var(--duration-stage-spin) var(--ease-stage-spin) infinite; }
  @keyframes spin { to { transform: rotate(-360deg); } }
  .note { margin: var(--s0); min-height: var(--s0); color: var(--secondary); }
  a { display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); }
</style>
