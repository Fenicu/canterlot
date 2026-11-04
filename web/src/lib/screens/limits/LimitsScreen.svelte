<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { onMount } from 'svelte';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { limitsHistory } from '#lib/api/client.js';
  import { ApiError } from '#lib/api/http.js';
  import { loadConfig } from '#lib/api/config.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import UsageChart from './UsageChart.svelte';
  import CurrentLimits from './CurrentLimits.svelte';
  import UsageTable from './UsageTable.svelte';
  import SpeedTable from './SpeedTable.svelte';
  import Button from '#lib/ui/Button.svelte';
  import { limitsSubtitle, sevenDays, usageRows, historyRange, speedRows, currentBlocks, historyCharts } from './model.js';
  import '../secondary.css';
  let runtime = $state<Runtime | null>(null);
  let series = $state<components['schemas']['ProviderSeries'][]>([]);
  let usage = $state<components['schemas']['UsageDay'][]>([]);
  let speed = $state<components['schemas']['ModelSpeedSummary'][]>([]);
  let days = $state<string[]>([]); let zone = $state(''); let error = $state(''); let loading = $state(true);
  const now = $derived.by(() => { void ticker.second; return clock.serverNow(); });
  let thresholds = $state({ stale_after_secs: 2100, warn_pcts: [80, 95], weekly_reserve_pct: 95 });
  const rows = $derived(usageRows(usage, days.at(-1) ?? '', runtime?.list.items ?? new Map()));
  const subtitle = $derived(limitsSubtitle(runtime?.list.limits ?? []));
  const blocks = $derived(currentBlocks(runtime?.list.limits ?? [], now, thresholds));
  const charts = $derived(historyCharts(series, days, zone));
  async function load() {
    loading = true; error = '';
    try {
      const [rt, cfg] = await Promise.all([browserRuntime(), loadConfig()]);
      runtime = rt; thresholds = cfg.limit_thresholds;
      zone = Intl.DateTimeFormat().resolvedOptions().timeZone; days = sevenDays(new Date(clock.serverNow()));
      const range = historyRange(new Date(clock.serverNow()));
      // История и скорость — по всем провайдерам; расход по сессиям остаётся у Claude и Codex.
      const replies = await Promise.all(['claude', 'codex', 'opencode', 'antigravity'].map(provider => limitsHistory({ provider, ...range, tz: zone })));
      series = replies.flatMap(r => r.data.series); usage = replies.slice(0, 2).flatMap(r => r.data.usage);
      speed = replies.flatMap(r => r.data.speed ?? []);
    } catch (err) { error = err instanceof ApiError ? `История недоступна: ${err.message}` : 'История недоступна: нет связи с хабом'; }
    finally { loading = false; }
  }
  onMount(() => { void load(); return ticker.retain(); });
</script>
<main id="main" tabindex="-1" data-secondary-ready={!loading ? '' : undefined}>
  <SecondaryHeader title="Лимиты" subtitle={subtitle} />
  <div class="secondary-body"><div class="secondary-content">
    <!-- Текущие лимиты приходят со снимком списка и видны, даже когда история не загрузилась. -->
    <h2>Сейчас</h2>
    {#if blocks.length}<CurrentLimits {blocks} />{:else if !loading}<p class="note">Хаб ещё не прислал лимитов</p>{/if}
    <h2>История лимитов</h2>
    {#if loading}<p>Загружаю историю лимитов…</p>{:else if error}<p class="error" role="alert">{error}</p><Button onclick={load}>Повторить</Button>
    {:else}
      <p class="note">{fmt.dayRange(days[0], days.at(-1))} · остаток недели на конец дня</p>
      {#if charts.length}<UsageChart {charts} />
      {:else}<p class="empty" data-history-empty>Снимков лимитов за эти дни ещё нет</p>{/if}
      <UsageTable {rows} /><SpeedTable rows={speedRows(speed)} />
    {/if}
  </div></div>
</main>
<style>
  .empty { margin: var(--s0); padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); color: var(--muted); }
</style>
