<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { loadConfig } from '#lib/api/config.js';
  import { pendingMachines, putMachineRemoteWork, revokePending } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { authApi } from '#lib/auth/api.js';
  import { stepUp, StepUpCancelled } from '#lib/auth/stepup.svelte.js';
  import StepUpPrompt from '#lib/auth/StepUpPrompt.svelte';
  import { clock } from '#lib/clock.js';
  import { connectionStale } from '#lib/sync/stale.svelte.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import { fmt } from '#lib/format/ru.js';
  import { credentialLine } from '#lib/stores/limits.js';
  import { modelLabel } from '#lib/format/labels.js';
  import Button from '#lib/ui/Button.svelte';
  import Pill from '#lib/ui/Pill.svelte';
  import Segmented from '#lib/ui/Segmented.svelte';
  import { swapOn } from '#lib/ui/swap.js';
  import SecondaryHeader from '../SecondaryHeader.svelte';
  import AddMachineSheet from './AddMachineSheet.svelte';
  import MetricCard from './MetricCard.svelte';
  import { diskLimits, diskView, generalWarnings, hotspotRows, journalStops, leakedSummary, machineStatus, memoryLowText, unleasedText } from './model.js';
  import { carrierLine, isRunner, remoteWorkText, runnerAvailability } from './runner.js';
  import LeaseQueue from './LeaseQueue.svelte';
  import JobSheet from './JobSheet.svelte';
  import JobsSection from './JobsSection.svelte';
  import CloudSection from './CloudSection.svelte';
  import { codeHint, sortPending, type PendingMachine } from './pending.js';
  import '../secondary.css';

  let runtime = $state<Runtime | null>(null);
  let selected = $state(''); let error = $state('');
  const now = $derived.by(() => { void ticker.second; return connectionStale.now(); });
  let config = $state({ disk_warn_pct: 85, disk_warn_free_gb: 30, disk_critical_free_gb: 10, metrics_stale_after_secs: 90 });
  // Ожидающие машины есть и в обычном списке: убираем их оттуда, чтобы не показывать offline.
  let pending = $state<PendingMachine[]>([]);
  let pendingError = $state(''); let pendingNotice = $state('');
  let methods = $state({ password: false, passkey: false });
  let addOpen = $state(false); let reissue = $state<PendingMachine | null>(null);
  let lastPendingMinute = -1;
  const pendingIds = $derived(new Set(pending.map((item) => item.machine_id)));
  const machines = $derived(runtime ? [...runtime.machines.items.values()].filter((m) => !pendingIds.has(m.id)) : []);
  const sortedPending = $derived(sortPending(pending));
  const pendingNow = $derived.by(() => { void ticker.second; return connectionStale.now() + clock.offset; });
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
  const titleOf = (id: string) => runtime?.list.items.get(id)?.title ?? machine?.sessions?.find(session => session.conversation_id === id)?.title;
  const runner = $derived(machine ? isRunner(machine) : false);
  const runnerState = $derived(machine && runner ? runnerAvailability(machine, now) : null);
  const parent = $derived(machine?.parent_machine_id ? machines.find(m => m.id === machine.parent_machine_id) : undefined);
  const carrier = $derived(machine ? carrierLine(machine, machines) : null);
  const remoteText = $derived(remoteWorkText(machine ?? {}));
  const heartbeat = $derived(machine?.heartbeat_age_ms != null ? (machine.heartbeat_received_at === undefined ? now : machine.heartbeat_received_at + clock.offset) - machine.heartbeat_age_ms : metrics ? Date.parse(metrics.observed_at) : null);

  // Лист задачи: ссылка из карточки ленты `?job=<id>` открывает его, а экран встаёт на машину задачи.
  let openJob = $state<string | null>(null);
  const linkedJob = $derived(page.url.searchParams.get('job'));
  $effect(() => {
    const id = linkedJob;
    if (id) untrack(() => { openJob = id; });
  });
  $effect(() => {
    const machineId = openJob ? runtime?.machines.jobs.get(openJob)?.machine_id : undefined;
    if (machineId && untrack(() => selected) !== machineId && machines.some(m => m.id === machineId)) selected = machineId;
  });
  // Ссылка на карточку другой машины `?machine=<id>`: родитель раннера и раннеры несущей.
  const linkedMachine = $derived(page.url.searchParams.get('machine'));
  $effect(() => {
    const id = linkedMachine;
    if (id && machines.some(m => m.id === id)) untrack(() => { selected = id; });
  });
  function openMachine(event: MouseEvent, id: string) {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
    event.preventDefault();
    selected = id;
  }
  function closeJob() {
    openJob = null;
    // Ссылка снимается: тот же переход из карточки ленты откроет лист снова.
    if (page.url.searchParams.has('job')) void goto(resolve('/machines'), { replaceState: true });
  }

  // Выключатель удалённой работы: пока ответ не пришёл, переключатель занят; отказ возвращает прежнее значение.
  let remoteBusy = $state(false); let remoteNotice = $state('');
  async function setRemoteWork(input: HTMLInputElement) {
    const target = machine;
    if (!target || !runtime || remoteBusy) { input.checked = target?.accepts_remote_work === true; return; }
    const next = input.checked; remoteBusy = true; remoteNotice = '';
    try {
      const { data } = await putMachineRemoteWork(target.id, { accepts_remote_work: next }, crypto.randomUUID());
      const current = runtime.machines.items.get(target.id) ?? target;
      runtime.machines.items.set(target.id, { ...current, accepts_remote_work: data.accepts_remote_work });
    } catch {
      input.checked = !next;
      remoteNotice = 'Не удалось изменить настройку. Повторите при восстановлении связи';
    } finally { remoteBusy = false; }
  }
  async function load() {
    error = '';
    try { const [value, cfg] = await Promise.all([browserRuntime(), loadConfig()]); runtime = value; config = cfg.machines; }
    catch { error = 'Не удалось загрузить машины. Повторите при восстановлении связи'; }
  }
  async function loadPending() {
    try { pending = (await pendingMachines()).data.items; pendingError = ''; }
    catch { pendingError = 'Не удалось загрузить ожидающие машины'; }
  }
  async function loadMethods() {
    // Без сведений о способах входа окно подтверждения просто не предложит пароль или ключ.
    try { methods = await authApi.methods(); } catch { /* подтверждение покажет ошибку хаба */ }
  }
  function openAdd() { pendingNotice = ''; addOpen = true; }
  function openReissue(item: PendingMachine) { pendingNotice = ''; reissue = item; }
  function closeSheet() { addOpen = false; reissue = null; }
  function issued() { void loadPending(); }
  async function revoke(item: PendingMachine) {
    pendingNotice = '';
    try {
      await stepUp.run(() => revokePending(item.machine_id, crypto.randomUUID()));
      void loadPending();
    } catch (err) {
      if (!(err instanceof StepUpCancelled)) pendingNotice = errorText(err, 'Не удалось отозвать машину. Повторите при восстановлении связи');
    }
  }
  onMount(() => {
    void load(); void loadMethods();
    return ticker.retain();
  });
  // Хаб отдаёт ожидающие машины по запросу: обновляем раз в минуту и после действий владельца.
  $effect(() => {
    const minute = Math.floor(ticker.second / 60);
    if (minute === lastPendingMinute) return;
    lastPendingMinute = minute;
    void loadPending();
  });
  function loadText(value: number | undefined) { return value === undefined ? 'Нет данных' : (value / 100).toLocaleString('ru-RU', { minimumFractionDigits: 2 }); }
</script>
<main id="main" tabindex="-1" data-secondary-ready={runtime ? '' : undefined}>
  <SecondaryHeader title="Машины" subtitle="Состояние ресурсов" />
  <div class="secondary-body"><div class="secondary-content">
    {#if error}<p class="error">{error}</p><button type="button" onclick={load}>Повторить</button>
    {:else}
      <div class="add-machine"><Button variant="primary" onclick={openAdd}>Добавить машину</Button></div>
      {#if pendingError}<p class="error" role="alert">{pendingError}</p>{/if}
      {#if sortedPending.length}
        <section class="pending" aria-labelledby="pending-title">
          <h2 id="pending-title">Ожидают подключения</h2>
          <ul>
            {#each sortedPending as item (item.machine_id)}
              {@const hint = codeHint(item, pendingNow)}
              <li>
                <span class="name">{item.name}</span>
                {#if hint}<span class="hint" data-expired={hint === 'Код истёк' ? '' : undefined}>{hint}</span>{/if}
                <div class="pending-actions">
                  <button type="button" onclick={() => openReissue(item)}>Новый код</button>
                  <button type="button" onclick={() => void revoke(item)}>Отозвать</button>
                </div>
              </li>
            {/each}
          </ul>
        </section>
      {/if}
      {#if pendingNotice}<p class="error" role="alert">{pendingNotice}</p>{/if}
      {#if !runtime || runtime.machines.status === 'idle' || runtime.machines.status === 'loading_snapshot'}<p>Загружаю машины…</p>
      {:else if !machine}<p>Машины ещё не подключены. Подключите воркер к хабу</p>
      {:else}
      {#if machines.length > 1}<Segmented name="machine" label="Машина" options={machines.map(m => ({ value: m.id, label: m.availability === 'degraded' ? `${m.name} · нестабильно` : m.availability === 'unavailable' ? `${m.name} · не в сети` : m.availability === 'offline' && isRunner(m) ? `${m.name} · недоступен` : m.name }))} bind:value={selected} />{/if}
      <CloudSection {runtime} {now} />
      <div class="heading"><h2>{machine.name}</h2>{#key machine.id}
        {#if runnerState}<span class="availability" data-availability={machine.availability} data-tone={runnerState.tone} data-runner-availability use:swapOn={{ text: runnerState.text, key: machine.availability }}></span>
        {:else}<span class="availability" data-availability={machine.availability} use:swapOn={{ text: machineStatus(machine.availability), key: machine.availability }}></span>{/if}{/key}</div>
      {#if runner}
        <div class="runner-facts" data-runner>
          <div class="runner-line">
            <Pill color="accent"><span data-runner-badge>Раннер</span></Pill>
            {#if parent}<span data-runner-parent>Несущая: <a href={resolve(`/machines?machine=${parent.id}` as '/machines')} onclick={(event) => openMachine(event, parent.id)}>{parent.name}</a></span>{/if}
            <span data-runner-rank>Ранг {machine.rank ?? 0}</span>
          </div>
          {#if machine.labels?.length}<ul class="labels" aria-label="Метки раннера">{#each machine.labels as label (label)}<li><Pill>{label}</Pill></li>{/each}</ul>{/if}
        </div>
      {/if}
      {#if carrier}<p class="note" data-carrier-line>Несущая раннеров: {#each carrier.runners as item, index (item.id)}{index ? ', ' : ''}<a href={resolve(`/machines?machine=${item.id}` as '/machines')} onclick={(event) => openMachine(event, item.id)}>{item.name}</a>{:else}раннеров пока нет{/each}</p>{/if}
      {#if machine.os}<p class="note">{({ linux: 'Linux', darwin: 'macOS', windows: 'Windows' } as Record<string, string>)[machine.os] ?? machine.os}</p>{/if}
      {#if machine.worker_version}<p class="note">Версия воркера {machine.worker_version}</p>{/if}
      {#each machine.credentials ?? [] as credential (credential.provider)}
        {@const line = credentialLine(credential.provider, credential.expires_at, now)}
        {#if line}<p class="credential" data-provider={credential.provider} data-tone={line.tone}>{line.text}</p>{/if}
      {/each}
      <section class="remote" aria-labelledby="remote-work-label" data-remote-work>
        <label class="switch">
          <input type="checkbox" role="switch" checked={machine.accepts_remote_work === true} disabled={remoteBusy}
            aria-labelledby="remote-work-label" aria-describedby="remote-work-note"
            onchange={(event) => void setRemoteWork(event.currentTarget)} />
          <span class="track"></span>
          <span id="remote-work-label">{remoteText.label}</span>
        </label>
        <p class="note" id="remote-work-note" data-remote-work-note>{remoteText.hint}</p>
        {#if remoteNotice}<p class="error" role="alert">{remoteNotice}</p>{/if}
      </section>
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
      <LeaseQueue lease={machine.lease} available={machine.availability === 'available' || machine.availability === 'online'} {titleOf} {now} />
      {#if runtime}<JobsSection {runtime} machineId={machine.id} onopen={(id) => { openJob = id; }} />{/if}
      {#if !runner}
        <h2>Сессии на машине</h2>
        {#each sessions as session (session.id)}<a class="session-link" href={resolve('/c/[id]', { id: session.id })}>{session.title}{#if session.detail}<small>{session.detail.harness.title} · {modelLabel(session.detail.model)}</small>{/if}</a>{:else}<p class="note">На этой машине пока нет сессий</p>{/each}
      {/if}
      <div class="heartbeat"><p>Последний сигнал {heartbeat === null ? 'ещё не получен' : fmt.ago(heartbeat, now)}</p>
      {#if machine.availability_since}<p class="note">Подключение с {fmt.clock(Date.parse(machine.availability_since))} · {fmt.dayLabel(Date.parse(machine.availability_since), now)}</p>{/if}</div>
      {/if}
    {/if}
  </div></div>
  {#if openJob && runtime}{#key openJob}<JobSheet {runtime} jobId={openJob} onclose={closeJob} />{/key}{/if}
  {#if addOpen}<AddMachineSheet onclose={closeSheet} onissued={issued} />{/if}
  {#if reissue}<AddMachineSheet machine={reissue} onclose={closeSheet} onissued={issued} />{/if}
  <StepUpPrompt password={methods.password} passkey={methods.passkey} />
</main>
<style>
  /* Снизу тот же шаг, что между карточками экрана: без него кнопка прилипала к вкладкам машин. */
  .add-machine { display: flex; margin-block: var(--s6) var(--s4); }
  .pending { background: var(--surface); border-radius: var(--radius-card); padding: var(--s5) var(--s6); margin-block: var(--s4); }
  .pending h2 { margin: var(--s0); font-size: var(--fs-body); font-weight: var(--weight-medium); }
  .pending ul { list-style: none; margin: var(--s4) 0 0; padding: 0; }
  .pending li { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s3) var(--s6); min-height: var(--target); padding-block: var(--s3); }
  .pending .name { min-width: 0; overflow-wrap: anywhere; }
  .pending .hint { color: var(--muted); font-size: var(--fs-meta); }
  .pending .hint[data-expired] { color: var(--warning); }
  .pending-actions { display: flex; flex-wrap: wrap; gap: var(--s3); margin-inline-start: auto; }
  .pending-actions button { min-height: var(--target); padding: var(--s0) var(--s3); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-meta); }
  .heading { display: flex; flex-wrap: wrap; gap: var(--s4); justify-content: space-between; align-items: center; margin-top: var(--s8); }
  .heading h2 { margin: 0; font-size: var(--fs-heading); }
  /* Состояние — подпись цветом, а не заливная капсула, которую легко принять за кнопку. */
  .availability { color: var(--success); font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  [data-availability='degraded'], .notice { color: var(--warning); }
  [data-availability='unavailable'] { color: var(--muted); }
  /* Раннер: тон несёт слово доступности, цвет только подкрепляет его. */
  .availability[data-tone='success'] { color: var(--success); }
  .availability[data-tone='danger'] { color: var(--danger); }
  .availability[data-tone='muted'] { color: var(--muted); }
  .runner-facts { display: flex; flex-direction: column; gap: var(--s3); margin-block: var(--s3); }
  .runner-line { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s4) var(--s6); color: var(--muted); font-size: var(--fs-meta); }
  .runner-line a, [data-carrier-line] a { color: var(--accent-link); }
  .labels { display: flex; flex-wrap: wrap; gap: var(--s3); list-style: none; margin: 0; padding: 0; }
  .stale { color: var(--muted); }
  /* Конец входа провайдера: дата на карточке машины, тревогу несут слова. */
  .credential { color: var(--muted); font-size: var(--fs-meta); }
  .credential[data-tone='warning'] { color: var(--warning); }
  .credential[data-tone='danger'] { color: var(--danger); }
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
  /* Выключатель — одна поверхность с подписью; зависит от владельца, а не от метрик. */
  .remote { background: var(--surface); border-radius: var(--radius-card); padding: var(--s5) var(--s6); margin-block: var(--s4); }
  .remote .note { margin: var(--s3) 0 0; color: var(--muted); font-size: var(--fs-meta); }
  .remote .error { margin: var(--s3) 0 0; color: var(--danger); font-size: var(--fs-meta); }
  .switch { position: relative; display: flex; align-items: center; gap: var(--s6); min-height: var(--target); cursor: pointer; }
  .switch input { position: absolute; opacity: 0; width: var(--toggle-width); height: var(--toggle-height); margin: 0; cursor: pointer; }
  .track { flex: none; width: var(--toggle-width); height: var(--toggle-height); border-radius: var(--radius-pill); background: var(--control-line); padding: var(--s2); pointer-events: none; white-space: nowrap; }
  .track::after { content: ''; display: block; width: var(--toggle-knob); height: var(--toggle-knob); border-radius: var(--radius-round); background: var(--surface); }
  .switch input:checked + .track { background: var(--accent); }
  .switch input:checked + .track::after { margin-left: auto; background: var(--accent-ink); }
  .switch input:focus-visible + .track { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  .switch input:disabled + .track { opacity: 0.6; }
  .heartbeat { margin-block: var(--s6); padding: var(--s6); border-radius: var(--radius); background: var(--surface); font-size: var(--fs-sm); color: var(--muted); }
</style>
