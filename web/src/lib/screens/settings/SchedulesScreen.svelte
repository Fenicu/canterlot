<script lang="ts">
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { browserRuntime } from '#lib/app/browser.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { deleteSchedule, pauseSchedule, resumeSchedule, scheduleRuns } from '#lib/api/client.js';
  import Button from '#lib/ui/Button.svelte';
  import { SchedulesStore } from '#lib/stores/schedules.svelte.js';
  import ScheduleEditor from './ScheduleEditor.svelte';
  import ScheduleDetail from './ScheduleDetail.svelte';
  import { cronHuman, fireTime, outcomeWord, targetLabel, type ScheduleRun, type Schedule } from './schedules.js';

  const store = new SchedulesStore();
  let runtime = $state<Runtime | null>(null);
  let last = $state<Record<string, ScheduleRun | undefined>>({});
  let notice = $state('');
  let busy = $state('');
  let confirming = $state('');

  // Имена сессий для строки цели; пока список не пришёл, подпись не выдумывает название.
  const sessions = $derived(new Map([...(runtime?.list.items.values() ?? [])].map((item) => [item.id, { title: item.title }])));
  const edit = $derived(page.url.searchParams.get('edit'));
  const view = $derived(page.url.searchParams.get('view'));
  const href = (suffix = '') => `${resolve('/settings')}?c=schedules${suffix}`;
  const selected = $derived(store.items.find((item) => item.id === view) ?? null);
  const editing = $derived(store.items.find((item) => item.id === edit) ?? null);

  onMount(() => {
    let alive = true;
    void browserRuntime().then((value) => {
      if (!alive) return;
      runtime = value;
      store.open(value.connection);
    });
    return () => { alive = false; store.close(); };
  });

  async function loadLast() {
    const rows = await Promise.all(store.items.map(async (item) => {
      try {
        const page = await scheduleRuns(item.id, { limit: 1 });
        return [item.id, page.data.items[0]] as const;
      } catch {
        return [item.id, undefined] as const;
      }
    }));
    last = Object.fromEntries(rows);
  }

  $effect(() => {
    void store.items.length;
    void store.version;
    void loadLast();
  });

  async function toggle(item: Schedule) {
    if (busy) return;
    busy = item.id; notice = '';
    try {
      const next = item.enabled
        ? (await pauseSchedule(item.id, crypto.randomUUID())).data
        : (await resumeSchedule(item.id, crypto.randomUUID())).data;
      await store.load();
      if (!next.enabled && view === item.id) notice = 'Расписание поставлено на паузу';
    } catch {
      notice = 'Не удалось изменить расписание. Повторите при восстановлении связи';
    } finally { busy = ''; }
  }

  async function remove(item: Schedule) {
    if (busy) return;
    busy = item.id; notice = '';
    try {
      await deleteSchedule(item.id, crypto.randomUUID());
      confirming = '';
      await store.load();
      if (view === item.id || edit === item.id) location.assign(href());
    } catch {
      notice = 'Не удалось удалить расписание. Повторите при восстановлении связи';
    } finally { busy = ''; }
  }
</script>

<section data-schedules class="schedules" aria-label="Расписание">
  {#if edit === 'new' || editing}
    {#if runtime}
      <ScheduleEditor schedule={editing} {runtime} {store} done={(id) => location.assign(href(`&view=${encodeURIComponent(id)}`))} cancel={() => location.assign(href())} />
    {:else}<p>Открываю форму…</p>{/if}
  {:else if view && selected}
    {#if runtime}
      <ScheduleDetail schedule={selected} {store} {sessions} back={() => location.assign(href())} edit={() => location.assign(href(`&edit=${encodeURIComponent(selected.id)}`))} removed={() => { void store.load(); location.assign(href()); }} />
    {:else}<p>Открываю расписание…</p>{/if}
  {:else}
    <div class="head">
      <h2>Расписание</h2>
      <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
      <a class="add" href={href('&edit=new')}><Button variant="primary">Новое расписание</Button></a>
    </div>
    <p class="note">Повторяющиеся запуски: новый сеанс по маршруту, сообщение в существующую сессию или отчёт из сессии карточкой в её ленту. Пропуск при красных лимитах и пересечении</p>
    {#if notice}<p class="error" role="alert">{notice}</p>{/if}
    {#if store.error}<p class="error" role="alert">{store.error}<Button onclick={() => store.load()}>Повторить</Button></p>{/if}
    {#if store.items.length === 0 && !store.error}
      <p class="empty">Расписаний пока нет. Создайте первое, чтобы запускать повторяющиеся задачи без участия владельца</p>
    {/if}
    <div class="list" data-schedules-list>
      {#each store.items as item (item.id)}
        {@const run = last[item.id]}
        {@const target = targetLabel(item.target, sessions)}
        {@const outcome = run ? outcomeWord(run.state) : null}
        <div class="row" data-schedule-row={item.id}>
          <div class="row-main">
            <strong>{item.name}</strong>
            <span class="meta">{cronHuman(item.cron)}</span>
            <span class="meta" data-role="schedule-target">{target.kind}{target.session ? ` · ${target.session}` : ''}</span>
            <span class="meta">Далее: {fireTime(item.next_fire_at, item.timezone)} (пояс {item.timezone})</span>
            {#if outcome}
              <span class="outcome" data-tone={outcome.tone}>{outcome.word}</span>
            {:else}
              <span class="outcome" data-tone="neutral">Запусков ещё не было</span>
            {/if}
          </div>
          <div class="row-side">
            <label class="switch">
              <input type="checkbox" role="switch" checked={item.enabled}
                aria-label={`Включено: ${item.name}`} disabled={busy === item.id}
                onchange={() => void toggle(item)} />
              <span class="track"></span>
            </label>
            <!-- eslint-disable-next-line svelte/no-navigation-without-resolve -->
            <a class="open" href={href(`&view=${encodeURIComponent(item.id)}`)}>Открыть</a>
          </div>
        </div>
        {#if confirming === item.id}
          <div class="confirm" role="group" aria-label={`Удалить ${item.name}`}>
            <p>Удалить расписание вместе с историей запусков?</p>
            <div class="confirm-actions">
              <Button onclick={() => confirming = ''}>Отмена</Button>
              <Button variant="primary" {...(busy === item.id ? { disabled: true as const, reason: 'Удаляю…' } : { disabled: false as const })} onclick={() => void remove(item)}>Удалить</Button>
            </div>
          </div>
        {:else}
          <button type="button" class="danger-link" onclick={() => confirming = item.id}>Удалить расписание</button>
        {/if}
      {/each}
    </div>
  {/if}
</section>

<style>
  .schedules { display: grid; gap: var(--s4); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); flex-wrap: wrap; }
  h2 { margin: var(--s0); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  .note, .empty { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .error { margin: var(--s0); color: var(--danger); }
  .add { text-decoration: none; }
  .list { display: grid; gap: var(--s3); }
  .row { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--s4); padding: var(--s6); border-radius: var(--radius-card); background: var(--surface); }
  .row-main { display: flex; flex-direction: column; gap: var(--s2); min-width: 0; }
  .row-main strong { overflow-wrap: anywhere; }
  .meta { color: var(--secondary); font-size: var(--fs-sm); }
  .outcome { color: var(--muted); font-size: var(--fs-sm); font-weight: var(--weight-medium); }
  .outcome[data-tone='ok'] { color: var(--success); }
  .outcome[data-tone='warn'] { color: var(--warning); }
  .outcome[data-tone='danger'] { color: var(--danger); }
  .row-side { display: flex; flex-direction: column; align-items: flex-end; gap: var(--s2); flex-shrink: 0; }
  .switch { display: flex; align-items: center; min-height: var(--target); min-width: var(--target); cursor: pointer; }
  .switch input { position: absolute; opacity: 0; width: var(--toggle-width); height: var(--toggle-height); margin: 0; cursor: pointer; }
  .track { width: var(--toggle-width); height: var(--toggle-height); border-radius: var(--radius-pill); background: var(--control-line); padding: var(--s2); pointer-events: none; white-space: nowrap; }
  .track::after { content: ''; display: block; width: var(--toggle-knob); height: var(--toggle-knob); border-radius: var(--radius-round); background: var(--surface); }
  .switch input:checked + .track { background: var(--accent); }
  .switch input:checked + .track::after { margin-left: auto; background: var(--accent-ink); }
  .switch input:focus-visible + .track { outline: var(--focus-width) solid var(--accent); outline-offset: var(--s1); }
  .open { display: flex; align-items: center; min-height: var(--target); padding-inline: var(--s4); color: var(--accent-link); }
  .danger-link { min-height: var(--target); padding-inline: var(--s4); background: var(--transparent); color: var(--danger); font-size: var(--fs-sm); text-align: left; }
  .confirm { display: grid; gap: var(--s3); padding: var(--s5) var(--s6); border-radius: var(--radius-card); background: var(--raised); }
  .confirm p { margin: var(--s0); }
  .confirm-actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
</style>
