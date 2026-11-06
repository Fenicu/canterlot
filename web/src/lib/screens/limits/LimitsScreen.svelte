<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { onDestroy, onMount } from 'svelte';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { limitsHistory } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import { loadConfig } from '#lib/api/config.js';
  import { limitThresholds } from '#lib/stores/limitThresholds.svelte.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import UsageChart from './UsageChart.svelte';
  import CurrentLimits from './CurrentLimits.svelte';
  import UsageTable from './UsageTable.svelte';
  import SpeedTable from './SpeedTable.svelte';
  import Button from '#lib/ui/Button.svelte';
  import Icon from '#lib/ui/Icon.svelte';
  import { limitStrip } from '../sessions/limitsText.js';
  import { LimitsRefresh } from '#lib/stores/limitsRefresh.svelte.js';
  import { limitsSubtitle, sevenDays, usageRows, usageGroups, historyRange, speedRows, currentBlocks, historyCharts, alignCharts, providersWithWeekly } from './model.js';
  import '../secondary.css';
  let runtime = $state<Runtime | null>(null);
  let series = $state<components['schemas']['ProviderSeries'][]>([]);
  let usage = $state<components['schemas']['UsageDay'][]>([]);
  let speed = $state<components['schemas']['ModelSpeedSummary'][]>([]);
  let days = $state<string[]>([]); let zone = $state(''); let error = $state(''); let loading = $state(true);
  const now = $derived.by(() => { void ticker.second; return clock.serverNow(); });
  const weekly = $derived(providersWithWeekly(runtime?.list.limits ?? []));
  const groups = $derived(usageGroups(usageRows(usage, days.at(-1) ?? '', runtime?.list.items ?? new Map(), weekly)));
  const subtitle = $derived(limitsSubtitle(runtime?.list.limits ?? []));
  const blocks = $derived(currentBlocks(runtime?.list.limits ?? [], now, limitThresholds));
  const charts = $derived(alignCharts(historyCharts(series, days, zone), blocks));
  const strip = $derived(limitStrip(runtime?.list.limits ?? [], now, limitThresholds));
  const refresher = new LimitsRefresh();
  $effect(() => refresher.observe(strip.updatedAt));
  onDestroy(() => refresher.dispose());
  // Пока ждём показания, кнопка остаётся в порядке табуляции и объясняет себя подписью.
  const refreshState = $derived(refresher.pending ? { 'aria-disabled': true } : {});
  async function load() {
    loading = true; error = '';
    try {
      const [rt] = await Promise.all([browserRuntime(), loadConfig()]);
      runtime = rt;
      zone = Intl.DateTimeFormat().resolvedOptions().timeZone; days = sevenDays(new Date(clock.serverNow()));
      const range = historyRange(new Date(clock.serverNow()));
      // История, расход и скорость — по всем провайдерам.
      const replies = await Promise.all(['claude', 'codex', 'opencode', 'antigravity'].map(provider => limitsHistory({ provider, ...range, tz: zone })));
      series = replies.flatMap(r => r.data.series); usage = replies.flatMap(r => r.data.usage);
      speed = replies.flatMap(r => r.data.speed ?? []);
    } catch (err) { error = err instanceof ApiError ? `История недоступна: ${err.message}` : 'История недоступна: нет связи с хабом'; }
    finally { loading = false; }
  }
  onMount(() => { void load(); return ticker.retain(); });
</script>
<main id="main" class="limits-screen" tabindex="-1" data-secondary-ready={!loading ? '' : undefined}>
  <SecondaryHeader title="Лимиты" subtitle={subtitle} />
  <div class="secondary-body"><div class="secondary-content limits">
    <!-- Три вопроса сверху вниз: сколько осталось и когда сброс, как шла неделя, что съело сегодня. -->
    <section class="now" data-region="now">
      <div class="section-head">
        <h2>Сейчас</h2>
        {#if blocks.length}<span class="meta">{strip.updated || 'данных пока нет'}</span>
          <button type="button" class="refresh" class:pending={refresher.pending} aria-label={refresher.pending ? 'Обновляю лимиты' : 'Обновить лимиты'} {...refreshState} onclick={() => refresher.run(strip.updatedAt)}><Icon name="retry" />{refresher.pending ? 'Обновляю' : 'Обновить'}</button>{/if}
      </div>
      <p class="note status" role="status">{refresher.note}</p>
      <!-- Текущие лимиты приходят со снимком списка и видны, даже когда история не загрузилась. -->
      {#if blocks.length}<CurrentLimits {blocks} />{:else if !loading}<p class="empty">Хаб ещё не прислал лимитов</p>{/if}
    </section>
    <section class="week" data-region="week">
      <div class="section-head"><h2>История лимитов</h2>{#if !loading && !error}<span class="meta">{fmt.dayRange(days[0], days.at(-1))} · остаток недели на конец дня</span>{/if}</div>
      {#if loading}<p class="note">Загружаю историю лимитов…</p>{:else if error}<p class="error" role="alert">{error}</p><Button onclick={load}>Повторить</Button>
      {:else if charts.length}<UsageChart {charts} thresholds={limitThresholds} />
      {:else}<p class="empty" data-history-empty>Снимков лимитов за эти дни ещё нет</p>{/if}
    </section>
    {#if !loading && !error}
      <div class="today">
        <section class="usage" data-region="usage"><UsageTable {groups} /></section>
        <section class="speed" data-region="speed"><SpeedTable rows={speedRows(speed)} /></section>
      </div>
    {/if}
  </div></div>
</main>
<style>
  /* Экран лимитов — сводка, а не текст: колонка шире колонки чтения, шапка расширяется вместе с ней. */
  .limits-screen { --reading-max: calc(var(--index-max) * 4 / 3); }
  .limits { container: limits / inline-size; --limits-column: calc(var(--gauge-width) * 1.75); }
  .limits section { min-width: 0; }
  .section-head { display: flex; flex-wrap: wrap; align-items: center; column-gap: var(--s6); row-gap: var(--s1); margin-block: var(--s10) var(--s4); }
  .now .section-head { margin-top: var(--s4); }
  .section-head h2 { margin: var(--s0); }
  /* На телефоне время показания уходит под заголовок, а кнопка остаётся в его строке. */
  @container limits (max-width: 30rem) {
    .now .section-head .meta { order: 3; flex-basis: 100%; }
  }
  .meta { color: var(--muted); font-size: var(--fs-meta); font-variant-numeric: tabular-nums; }
  /* Кнопка обновления стоит в конце строки заголовка: рядом со временем показания, которое она освежает. */
  .secondary-content .refresh { display: inline-flex; align-items: center; gap: var(--s3); margin-inline-start: auto; padding-inline: var(--s6) var(--s8); font-size: var(--fs-meta); }
  .refresh :global(svg) { width: var(--icon-small); height: var(--icon-small); }
  .refresh.pending { color: var(--muted); cursor: default; }
  .refresh.pending :global(svg) { animation: spin var(--duration-stage-spin) var(--ease-stage-spin) infinite; }
  @keyframes spin { to { transform: rotate(-360deg); } }
  .secondary-content .status { margin-block: var(--s0); }
  .secondary-content .status:not(:empty) { margin-block-end: var(--s4); }
  .empty { margin: var(--s0); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); color: var(--muted); font-size: var(--fs-meta); }
  /* Сегодняшний расход и скорость: на широком экране рядом, расход шире — в нём больше колонок. */
  .today { display: grid; grid-template-columns: minmax(0, 1fr); column-gap: var(--s12); }
  .today :global(h2) { margin-block: var(--s10) var(--s4); }
  @container limits (min-width: 50rem) {
    .today { grid-template-columns: minmax(0, 2fr) minmax(0, 1fr); }
  }
</style>
