<script lang="ts">
  import { fmt } from '#lib/format/ru.js';
  import { onMount } from 'svelte';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { limitsHistory } from '#lib/api/client.js';
  import { loadConfig } from '#lib/api/config.js';
  import type { components } from '#lib/api/generated/openapi.js';
  import { limitView } from '#lib/stores/limits.js';
  import { clock } from '#lib/clock.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import UsageChart from './UsageChart.svelte';
  import UsageTable from './UsageTable.svelte';
  import { providerName, sevenDays, usageRows, historyRange } from './model.js';
  import '../secondary.css';
  let runtime = $state<Runtime | null>(null);
  let series = $state<components['schemas']['ProviderSeries'][]>([]);
  let usage = $state<components['schemas']['UsageDay'][]>([]);
  let days = $state<string[]>([]); let zone = $state(''); let error = $state(''); let loading = $state(true);
  let now = $state(clock.serverNow());
  let thresholds = $state({ stale_after_secs: 2100, warn_pcts: [80, 95], weekly_reserve_pct: 95 });
  const rows = $derived(usageRows(usage, days.at(-1) ?? '', runtime?.list.items ?? new Map()));
  async function load() {
    loading = true; error = '';
    try {
      const [rt, cfg] = await Promise.all([browserRuntime(), loadConfig()]);
      runtime = rt; thresholds = cfg.limit_thresholds;
      zone = Intl.DateTimeFormat().resolvedOptions().timeZone; days = sevenDays(new Date(clock.serverNow()));
      const range = historyRange(new Date(clock.serverNow()));
      const replies = await Promise.all(['claude', 'codex'].map(provider => limitsHistory({ provider, ...range, tz: zone })));
      series = replies.flatMap(r => r.data.series); usage = replies.flatMap(r => r.data.usage);
    } catch { error = 'Не удалось загрузить историю лимитов. Повторите при восстановлении связи'; }
    finally { loading = false; }
  }
  onMount(() => { void load(); const timer = setInterval(() => now = clock.serverNow(), 1000); return () => clearInterval(timer); });
</script>
<main id="main" tabindex="-1" data-secondary-ready={!loading ? '' : undefined}>
  <SecondaryHeader title="Лимиты" subtitle="Claude Code и Codex" />
  <div class="secondary-body"><div class="secondary-content">
    <h2>История лимитов</h2>
    {#if loading}<p>Загружаю историю лимитов…</p>{:else if error}<p class="error">{error}</p><button type="button" onclick={load}>Повторить</button>
    {:else}
      <p class="note">{fmt.dayRange(days[0], days.at(-1))} · остаток квоты</p>
      <UsageChart {series} {days} {zone} />
      <p class="note">Снимок в конце дня · сегодня по последним данным хаба</p>
      {#if !series.some(p => p.five_hour.length || p.weekly.length)}<p>Снимков лимитов за эти дни ещё нет</p>{/if}
      <div class="current">{#each runtime?.list.limits ?? [] as provider (provider.provider)}
        {@const view = limitView(provider, now, thresholds)}
        <section data-tone={view.tone}><p class="note">{providerName(provider.provider)} сейчас · осталось</p><strong>{view.five.text} / 5ч</strong><p>{view.week.known ? `${view.week.text} недели` : 'Нет данных недели'}</p>{#if view.week.reset}<small>{view.week.reset}</small>{/if}{#if view.stale}<p class="note">данные устарели</p>{/if}</section>
      {/each}</div>
      <UsageTable {rows} />
    {/if}
  </div></div>
</main>
<style>
  .current { display: flex; flex-wrap: wrap; gap: var(--s8); margin-block: var(--s8); }
  .current section { flex: 1; padding: var(--s8); background: var(--surface); border-radius: var(--radius-card); }
  strong { font-size: var(--fs-heading); }
  small { color: var(--muted); }
  [data-tone='warning'] > p:not(.note), [data-tone='reserve'] > p:not(.note) { color: var(--warning); }
  [data-tone='danger'] > p:not(.note) { color: var(--danger); }
</style>
