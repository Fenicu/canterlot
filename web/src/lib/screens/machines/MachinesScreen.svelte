<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { browserRuntime } from '#lib/screens/chat/open.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { loadConfig } from '#lib/api/config.js';
  import { clock } from '#lib/clock.js';
  import { fmt } from '#lib/format/ru.js';
  import { modelLabel } from '#lib/screens/chat/settingsLabels.js';
  import Segmented from '#lib/ui/Segmented.svelte';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import MetricCard from './MetricCard.svelte';
  import { diskView, machineStatus, machineWarnings } from './model.js';
  import '../secondary.css';

  let runtime = $state<Runtime | null>(null);
  let selected = $state(''); let now = $state(clock.serverNow()); let error = $state('');
  let config = $state({ disk_warn_pct: 85, metrics_stale_after_secs: 90 });
  const machines = $derived(runtime ? [...runtime.machines.items.values()] : []);
  $effect(() => { if (!machines.some(m => m.id === selected)) selected = machines[0]?.id ?? ''; });
  const machine = $derived(machines.find(m => m.id === selected) ?? machines[0]);
  const metrics = $derived(machine?.metrics);
  const stale = $derived(Boolean(metrics && (now - Date.parse(metrics.observed_at) > config.metrics_stale_after_secs * 1000 || machine?.warnings.includes('metrics_stale'))));
  const memory = $derived(metrics?.mem_total_bytes);
  const usedMemory = $derived(memory !== undefined && metrics?.mem_available_bytes !== undefined ? memory - metrics.mem_available_bytes : null);
  const disks = $derived((metrics?.disks ?? []).map(disk => ({ ...disk, ...diskView(disk, machine?.allowed_roots ?? [], config.disk_warn_pct) })));
  const warnings = $derived(machineWarnings([...new Set([...(machine?.warnings ?? []), ...(disks.some(d => d.warn) ? ['disk_usage_high'] : [])])]));
  const sessions = $derived(machine?.sessions?.map(session => ({ id: session.conversation_id, title: session.title, detail: runtime?.list.items.get(session.conversation_id) })) ?? (runtime && machine ? [...runtime.list.items.values()].filter(s => s.machine.id === machine.id).map(session => ({ id: session.id, title: session.title, detail: session })) : []));
  const heartbeat = $derived(machine?.heartbeat_age_ms != null ? (machine.heartbeat_received_at === undefined ? now : machine.heartbeat_received_at + clock.offset) - machine.heartbeat_age_ms : metrics ? Date.parse(metrics.observed_at) : null);

  async function load() {
    error = '';
    try { const [value, cfg] = await Promise.all([browserRuntime(), loadConfig()]); runtime = value; config = cfg.machines; }
    catch { error = 'Не удалось загрузить машины. Повторите при восстановлении связи'; }
  }
  onMount(() => { void load(); const timer = setInterval(() => now = clock.serverNow(), 1000); return () => clearInterval(timer); });
  function loadText(value: number | undefined) { return value === undefined ? 'Нет данных' : (value / 100).toLocaleString('ru-RU', { minimumFractionDigits: 2 }); }
</script>
<main id="main" tabindex="-1" data-secondary-ready={runtime ? '' : undefined}>
  <SecondaryHeader title="Машины" subtitle="Состояние ресурсов" />
  <div class="secondary-body"><div class="secondary-content">
    {#if error}<p class="error">{error}</p><button type="button" onclick={load}>Повторить</button>
    {:else if !runtime || runtime.machines.status === 'idle' || runtime.machines.status === 'loading_snapshot'}<p>Загружаю машины…</p>
    {:else if !machine}<p>Машины ещё не подключены. Подключите воркер к хабу</p>
    {:else}
      {#if machines.length > 1}<Segmented name="machine" label="Машина" options={machines.map(m => ({ value: m.id, label: m.name }))} bind:value={selected} />{/if}
      <div class="heading"><h2>{machine.name}</h2><span class="availability" data-availability={machine.availability}>{machineStatus(machine.availability)}</span></div>
      {#if machine.os}<p class="note">{({ linux: 'Linux', darwin: 'macOS', windows: 'Windows' } as Record<string, string>)[machine.os] ?? machine.os}</p>{/if}
      {#if machine.availability === 'unavailable'}<p class="notice">Машина не в сети. Данные от последнего сигнала</p>{/if}
      {#if machine.availability === 'degraded'}<p class="notice">Связь нестабильна. Показаны последние полученные данные</p>{/if}
      <div data-stale={stale} class:stale>
        {#if stale}<p>данные устарели</p>{/if}
        {#if metrics}
          <MetricCard title="CPU" value={metrics.cpu_permille === undefined ? 'Нет данных' : fmt.pct(metrics.cpu_permille)} pct={metrics.cpu_permille === undefined ? null : metrics.cpu_permille / 10} caption={metrics.cpu_cores ? `${metrics.cpu_cores} ${fmt.plural(metrics.cpu_cores, ['ядро', 'ядра', 'ядер'])}` : ''} />
          <MetricCard title="RAM" value={usedMemory !== null && memory ? `${fmt.bytes(usedMemory)} / ${fmt.bytes(memory)}` : 'Нет данных'} pct={usedMemory !== null && memory ? Math.round(usedMemory / memory * 1000) / 10 : null} caption={metrics.mem_available_bytes === undefined ? '' : `Доступно ${fmt.bytes(metrics.mem_available_bytes)}`} />
          {#each disks as disk (disk.label)}
            <MetricCard title={disk.label} value={disk.pct === null ? 'Нет данных' : fmt.pct(Math.round(disk.pct * 10))} pct={disk.pct} warn={disk.warn} caption={`Диск · ${fmt.bytes(disk.total_bytes - disk.free_bytes)} из ${fmt.bytes(disk.total_bytes)} · свободно ${fmt.bytes(disk.free_bytes)}`} />
          {/each}
          {#each warnings as warning (warning)}<p class="notice">{warning}</p>{/each}
          <MetricCard title="Нагрузка" value={loadText(metrics.load1_centi)} caption={`1 / 5 / 15 мин: ${loadText(metrics.load1_centi)} · ${loadText(metrics.load5_centi)} · ${loadText(metrics.load15_centi)}`} />
        {:else}<p>Метрики ещё не поступили. Ожидаю сигнал машины</p>{/if}
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
  .availability { border-radius: var(--radius-pill); background: var(--raised); padding: var(--s3) var(--s6); color: var(--success); }
  [data-availability='degraded'], .notice { color: var(--warning); }
  [data-availability='unavailable'] { color: var(--muted); }
  .stale { color: var(--muted); }
  .stale :global(strong) { color: var(--muted); }
  .stale :global([role='meter'] span) { background: var(--muted); }
  .session-link { display: block; padding: var(--s8); margin-bottom: var(--s4); background: var(--surface); border-radius: var(--radius-card); min-height: var(--target); }
  small { display: block; color: var(--muted); }
  .heartbeat { margin-block: var(--s6); padding: var(--s6); border-radius: var(--radius); background: var(--surface); font-size: var(--fs-sm); color: var(--muted); }
</style>
