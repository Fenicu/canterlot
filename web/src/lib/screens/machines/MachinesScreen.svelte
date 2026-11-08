<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { loadConfig } from '#lib/api/config.js';
  import { clock } from '#lib/clock.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { fmt } from '#lib/format/ru.js';
  import { modelLabel } from '#lib/format/labels.js';
  import Segmented from '#lib/ui/Segmented.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import MetricCard from './MetricCard.svelte';
  import { diskLimits, diskView, generalWarnings, hotspotRows, journalStops, leakedSummary, machineStatus, memoryLowText, unleasedText } from './model.js';
  import '../secondary.css';

  let runtime = $state<Runtime | null>(null);
  let selected = $state(''); let error = $state('');
  const now = $derived.by(() => { void ticker.second; return clock.serverNow(); });
  let config = $state({ disk_warn_pct: 85, disk_warn_free_gb: 30, disk_critical_free_gb: 10, metrics_stale_after_secs: 90 });
  const machines = $derived(runtime ? [...runtime.machines.items.values()] : []);
  $effect(() => { if (!machines.some(m => m.id === selected)) selected = machines[0]?.id ?? ''; });
  const machine = $derived(machines.find(m => m.id === selected) ?? machines[0]);
  const metrics = $derived(machine?.metrics);
  const stale = $derived(Boolean(metrics && (now - Date.parse(metrics.observed_at) > config.metrics_stale_after_secs * 1000 || machine?.warnings.includes('metrics_stale'))));
  const limits = $derived(diskLimits(config));
  const memory = $derived(metrics?.mem_total_bytes);
  const usedMemory = $derived(memory !== undefined && metrics?.mem_available_bytes !== undefined ? memory - metrics.mem_available_bytes : null);
  const disks = $derived((metrics?.disks ?? []).map(disk => ({ ...disk, ...diskView(disk, machine?.allowed_roots ?? [], limits) })));
  const warnings = $derived(generalWarnings([...new Set([...(machine?.warnings ?? []).filter(w => w !== 'disk_usage_high'), ...(disks.some(d => d.tone !== 'normal') ? ['disk_usage_high'] : [])])]));
  const watch = $derived(machine?.watch ?? null);
  const memoryLow = $derived(machine?.warnings.includes('memory_low') ?? false);
  const hotspots = $derived(watch ? hotspotRows(watch.hotspots, now) : []);
  const leaked = $derived(watch && machine?.warnings.includes('processes_leaked') ? leakedSummary(watch.leaked) : null);
  const unleased = $derived(watch ? unleasedText(watch.unleased_count) : null);
  const sessions = $derived(machine?.sessions?.map(session => ({ id: session.conversation_id, title: session.title, detail: runtime?.list.items.get(session.conversation_id) })) ?? (runtime && machine ? [...runtime.list.items.values()].filter(s => s.machine.id === machine.id).map(session => ({ id: session.id, title: session.title, detail: session })) : []));
  const heartbeat = $derived(machine?.heartbeat_age_ms != null ? (machine.heartbeat_received_at === undefined ? now : machine.heartbeat_received_at + clock.offset) - machine.heartbeat_age_ms : metrics ? Date.parse(metrics.observed_at) : null);

  async function load() {
    error = '';
    try { const [value, cfg] = await Promise.all([browserRuntime(), loadConfig()]); runtime = value; config = cfg.machines; }
    catch { error = 'Не удалось загрузить машины. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); return ticker.retain(); });
  function loadText(value: number | undefined) { return value === undefined ? 'Нет данных' : (value / 100).toLocaleString('ru-RU', { minimumFractionDigits: 2 }); }
</script>
<main id="main" tabindex="-1" data-secondary-ready={runtime ? '' : undefined}>
  <SecondaryHeader title="Машины" subtitle="Состояние ресурсов" />
  <div class="secondary-body"><div class="secondary-content">
    {#if error}<p class="error">{error}</p><button type="button" onclick={load}>Повторить</button>
    {:else if !runtime || runtime.machines.status === 'idle' || runtime.machines.status === 'loading_snapshot'}<p>Загружаю машины…</p>
    {:else if !machine}<p>Машины ещё не подключены. Подключите воркер к хабу</p>
    {:else}
      {#if machines.length > 1}<Segmented name="machine" label="Машина" options={machines.map(m => ({ value: m.id, label: m.availability === 'degraded' ? `${m.name} · нестабильно` : m.availability === 'unavailable' ? `${m.name} · не в сети` : m.name }))} bind:value={selected} />{/if}
      <div class="heading"><h2>{machine.name}</h2><span class="availability" data-availability={machine.availability}>{machineStatus(machine.availability)}</span></div>
      {#if machine.os}<p class="note">{({ linux: 'Linux', darwin: 'macOS', windows: 'Windows' } as Record<string, string>)[machine.os] ?? machine.os}</p>{/if}
      {#if machine.worker_version}<p class="note">Версия воркера {machine.worker_version}</p>{/if}
      {#if machine.availability === 'unavailable'}<p class="notice">Машина не в сети. Данные от последнего сигнала</p>{/if}
      {#if machine.availability === 'degraded'}<p class="notice">Связь нестабильна. Показаны последние полученные данные</p>{/if}
      <div data-stale={stale} class:stale>
        {#if stale}<p>данные устарели</p>{/if}
        {#if metrics}
          <MetricCard title="CPU" value={metrics.cpu_permille === undefined ? 'Нет данных' : fmt.pct(metrics.cpu_permille)} pct={metrics.cpu_permille === undefined ? null : metrics.cpu_permille / 10} caption={metrics.cpu_cores ? `${metrics.cpu_cores} ${fmt.plural(metrics.cpu_cores, ['ядро', 'ядра', 'ядер'])}` : ''} />
          <MetricCard title="RAM" value={usedMemory !== null && memory ? `${fmt.bytes(usedMemory)} / ${fmt.bytes(memory)}` : 'Нет данных'} pct={usedMemory !== null && memory ? Math.round(usedMemory / memory * 1000) / 10 : null} tone={memoryLow ? 'warning' : 'normal'} caption={metrics.mem_available_bytes === undefined ? '' : `Доступно ${fmt.bytes(metrics.mem_available_bytes)}`} />
          {#if memoryLow}<p class="notice" data-watch="memory">{memoryLowText(metrics.mem_available_bytes)}</p>{/if}
          {#each disks as disk (disk.label)}
            <MetricCard title={disk.label} value={disk.pct === null ? 'Нет данных' : fmt.pct(Math.round(disk.pct * 10))} pct={disk.pct} tone={disk.tone} caption={disk.text} />
          {/each}
          {#each warnings as warning (warning)}<p class="notice">{warning}</p>{/each}
          {#if hotspots.length}
            <section class="watch" aria-labelledby="watch-caches">
              <h3 id="watch-caches">Крупные кэши сборки</h3>
              <ul>
                {#each hotspots as spot (spot.key)}
                  <li><div class="row"><span class="name" class:path={!spot.path}>{spot.name}</span><strong>{spot.size}</strong></div>
                    {#if spot.path}<span class="path meta">{spot.path}</span>{/if}
                    {#if spot.meta}<span class="meta">{spot.meta}</span>{/if}</li>
                {/each}
              </ul>
              <p class="hint">Освободить: <code>scripts/prune-target.sh</code> или удалить <code>target/debug/incremental</code> в простаивающих копиях</p>
            </section>
          {/if}
          {#if leaked}
            <section class="watch" aria-labelledby="watch-leaked">
              <h3 id="watch-leaked" class="warned">{leaked.title}</h3>
              <p class="hint">{leaked.meta}</p>
              <ul>
                {#each leaked.rows as row (row.key)}<li><span class="name">{row.name}</span>{#each row.processes as process (process.key)}<span class="meta">{process.text}</span>{/each}</li>{/each}
              </ul>
            </section>
          {/if}
          {#if unleased}<p class="notice" data-watch="unleased">{unleased}</p>{/if}
          <MetricCard title="Нагрузка" value={loadText(metrics.load1_centi)} caption={`1 / 5 / 15 мин: ${loadText(metrics.load1_centi)} · ${loadText(metrics.load5_centi)} · ${loadText(metrics.load15_centi)}`} />
        {:else}
          <p>Метрики ещё не поступили. Ожидаю сигнал машины</p>
          {#each journalStops(machine?.warnings ?? []) as warning (warning)}<p class="notice">{warning}</p>{/each}
        {/if}
      </div>
      <h2>Сессии на машине</h2>
      {#each sessions as session (session.id)}<a class="session-link" href={resolve('/c/[id]', { id: session.id })}>{session.title}{#if session.detail}<small>{session.detail.harness.title} · {modelLabel(session.detail.model)}</small>{/if}</a>{:else}<p class="note">На этой машине пока нет сессий</p>{/each}
      <div class="heartbeat"><p>Последний сигнал {heartbeat === null ? 'ещё не получен' : fmt.ago(heartbeat, now)}</p>
      {#if machine.availability_since}<p class="note">Подключение с {fmt.clock(Date.parse(machine.availability_since))} · {fmt.dayLabel(Date.parse(machine.availability_since), now)}</p>{/if}</div>
    {/if}
  </div></div>
</main>
<style>
  .heading { display: flex; flex-wrap: wrap; gap: var(--s4); justify-content: space-between; align-items: center; margin-top: var(--s8); }
  .heading h2 { margin: 0; font-size: var(--fs-heading); }
  /* Состояние — подпись цветом, а не заливная капсула, которую легко принять за кнопку. */
  .availability { color: var(--success); font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  [data-availability='degraded'], .notice { color: var(--warning); }
  [data-availability='unavailable'] { color: var(--muted); }
  .stale { color: var(--muted); }
  .stale :global(strong) { color: var(--muted); }
  .stale :global([role='meter'] span) { background: var(--muted); }
  .session-link { display: block; padding: var(--s8); margin-bottom: var(--s4); background: var(--surface); border-radius: var(--radius-card); min-height: var(--target); }
  small { display: block; color: var(--muted); }
  /* Отчёт сторожа — та же поверхность, что у карточек метрик; тревогу несёт цвет текста. */
  .watch { background: var(--surface); border-radius: var(--radius-card); padding: var(--s5) var(--s6); margin-block: var(--s2); }
  .watch h3 { font-size: var(--fs-body); font-weight: var(--weight-medium); }
  .watch .warned { color: var(--warning); }
  .watch ul { list-style: none; margin: var(--s4) 0 0; padding: 0; }
  .watch li { display: flex; flex-direction: column; gap: var(--s1); padding-block: var(--s3); min-width: 0; }
  .watch .row { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s6); min-width: 0; }
  .watch .name { min-width: 0; overflow-wrap: anywhere; }
  .watch strong { flex: none; font-variant-numeric: tabular-nums; }
  .watch .path { font-family: var(--font-mono); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .watch .meta, .watch .hint { color: var(--muted); font-size: var(--fs-meta); }
  .watch .hint { margin-block: var(--s2) 0; }
  .heartbeat { margin-block: var(--s6); padding: var(--s6); border-radius: var(--radius); background: var(--surface); font-size: var(--fs-sm); color: var(--muted); }
</style>
