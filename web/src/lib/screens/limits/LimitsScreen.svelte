<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { onMount } from 'svelte';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { limitsHistory } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import { loadConfig } from '#lib/api/config.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { limitView } from '#lib/stores/limits.js';
  import { clock } from '#lib/clock.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import UsageChart from './UsageChart.svelte';
  import UsageTable from './UsageTable.svelte';
  import SpeedTable from './SpeedTable.svelte';
  import Button from '#lib/ui/Button.svelte';
  import { limitsSubtitle, providerName, sevenDays, usageRows, historyRange, speedRows } from './model.js';
  import '../secondary.css';
  let runtime = $state<Runtime | null>(null);
  let series = $state<components['schemas']['ProviderSeries'][]>([]);
  let usage = $state<components['schemas']['UsageDay'][]>([]);
  let speed = $state<components['schemas']['ModelSpeedSummary'][]>([]);
  let days = $state<string[]>([]); let zone = $state(''); let error = $state(''); let loading = $state(true);
  let now = $state(clock.serverNow());
  let thresholds = $state({ stale_after_secs: 2100, warn_pcts: [80, 95], weekly_reserve_pct: 95 });
  const rows = $derived(usageRows(usage, days.at(-1) ?? '', runtime?.list.items ?? new Map()));
  const subtitle = $derived(limitsSubtitle(runtime?.list.limits ?? []));
  async function load() {
    loading = true; error = '';
    try {
      const [rt, cfg] = await Promise.all([browserRuntime(), loadConfig()]);
      runtime = rt; thresholds = cfg.limit_thresholds;
      zone = Intl.DateTimeFormat().resolvedOptions().timeZone; days = sevenDays(new Date(clock.serverNow()));
      const range = historyRange(new Date(clock.serverNow()));
      // OpenCode нужен только для скорости: график и расход по сессиям остаются как были.
      const [claude, codex, opencode] = await Promise.all(['claude', 'codex', 'opencode'].map(provider => limitsHistory({ provider, ...range, tz: zone })));
      const replies = [claude!, codex!];
      series = replies.flatMap(r => r.data.series); usage = replies.flatMap(r => r.data.usage);
      speed = [...replies, opencode!].flatMap(r => r.data.speed ?? []);
    } catch (err) { error = err instanceof ApiError ? `История недоступна: ${err.message}` : 'История недоступна: нет связи с хабом'; }
    finally { loading = false; }
  }
  onMount(() => { void load(); const timer = setInterval(() => now = clock.serverNow(), 1000); return () => clearInterval(timer); });
</script>
<main id="main" tabindex="-1" data-secondary-ready={!loading ? '' : undefined}>
  <SecondaryHeader title="Лимиты" subtitle={subtitle} />
  <div class="secondary-body"><div class="secondary-content">
    <h2>История лимитов</h2>
    {#if loading}<p>Загружаю историю лимитов…</p>{:else if error}<p class="error" role="alert">{error}</p><Button onclick={load}>Повторить</Button>
    {:else}
      <p class="note">{fmt.dayRange(days[0], days.at(-1))} · остаток квоты</p>
      {#if series.some(p => p.five_hour.length || p.weekly.length)}
        <UsageChart {series} {days} {zone} noFiveHour={(runtime?.list.limits ?? []).filter(p => p.five_hour_absent).map(p => p.provider)} />
        <p class="note">Снимок в конце дня · сегодня по последним данным хаба</p>
      {:else}
        <p class="empty" data-history-empty>Снимков лимитов за эти дни ещё нет</p>
      {/if}
    {/if}
    <!-- Текущие лимиты приходят со снимком списка и видны, даже когда история не загрузилась. -->
    <div class="current">{#each runtime?.list.limits ?? [] as provider (provider.provider)}
      {@const view = limitView(provider, now, thresholds)}
      {@const primary = view.five.absent ? view.week : view.five}
      {@const windowLabel = view.five.absent ? 'нед' : '5ч'}
      <section data-tone={view.tone}>
        <p class="note">{providerName(provider.provider)} сейчас · осталось {#if provider.data_confidence === 'estimated'}· оценка{/if}</p>
        <strong class="value">{primary.text}&#32;/ <span class="window">{windowLabel}</span></strong>
        {#if !view.five.absent}<p class="other">{view.week.known ? `${view.week.text} недели` : 'Нет данных недели'}</p>{/if}
        {#if view.month}<p class="other">{['мес', view.month.text, view.month.reset].filter(Boolean).join(' · ')}</p>{/if}
        {#if view.week.reset}<small>{view.week.reset}</small>{/if}
        {#if view.stale}<p class="note">данные устарели</p>{/if}
      </section>
    {/each}</div>
    {#if !loading && !error}<UsageTable {rows} /><SpeedTable rows={speedRows(speed)} />{/if}
  </div></div>
</main>
<style>
  .current { display: flex; flex-wrap: wrap; gap: var(--s8); margin-block: var(--s8); }
  .empty { margin: var(--s0); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); color: var(--muted); }
  .current section { flex: 1 1 calc(var(--gauge-width) * 2); min-width: 0; padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  .value { display: block; white-space: nowrap; font-size: var(--fs-heading); }
  .window { font-size: var(--fs-body); font-weight: var(--weight-normal); }
  small { display: block; margin-top: var(--s2); color: var(--muted); }
  [data-tone='warning'] > p:not(.note), [data-tone='reserve'] > p:not(.note) { color: var(--warning); }
  [data-tone='danger'] > p:not(.note) { color: var(--danger); }
</style>
