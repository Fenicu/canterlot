<script lang="ts">
  import { resolve } from '$app/paths';
  import { deleteSchedule, pauseSchedule, resumeSchedule, runSchedule, scheduleRuns } from '#lib/api/client.js';
  import { fmt } from '#lib/format/ru.js';
  import type { SchedulesStore } from '#lib/stores/schedules.svelte.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Button from '#lib/ui/Button.svelte';
  import {
    cronHuman, fireTime, outcomeWord, runDetails, targetLabel, type Schedule, type ScheduleRun
  } from './schedules.js';

  let { schedule, store, sessions, back, edit, removed }: {
    schedule: Schedule;
    sessions: ReadonlyMap<string, { title: string }>;
    store: SchedulesStore;
    back: () => void;
    edit: () => void;
    removed: () => void;
  } = $props();

  let runs = $state<ScheduleRun[]>([]);
  let error = $state('');
  let notice = $state('');
  let busy = $state('');
  let confirming = $state(false);
  let runKey = crypto.randomUUID();

  async function loadRuns() {
    try {
      runs = (await scheduleRuns(schedule.id, { limit: 100 })).data.items;
      error = '';
    } catch {
      error = 'Не удалось загрузить историю запусков. Повторите при восстановлении связи';
    }
  }

  $effect(() => {
    void schedule.id;
    void store.version;
    void loadRuns();
  });

  async function runNow(anyway: boolean) {
    if (busy) return;
    busy = 'run'; notice = '';
    try {
      await runSchedule(schedule.id, { run_anyway: anyway }, runKey);
      // Новый ключ только после подтверждённого ответа: потерянный ответ повтор не дублирует.
      runKey = crypto.randomUUID();
      notice = anyway ? 'Запущено без проверок расписания' : 'Запуск принят';
      await loadRuns();
    } catch {
      notice = 'Не удалось запустить расписание. Повторите той же кнопкой';
    } finally { busy = ''; }
  }

  async function toggle() {
    if (busy) return;
    busy = 'toggle'; notice = '';
    try {
      const next = schedule.enabled
        ? (await pauseSchedule(schedule.id, crypto.randomUUID())).data
        : (await resumeSchedule(schedule.id, crypto.randomUUID())).data;
      notice = next.enabled ? 'Расписание возобновлено' : 'Расписание поставлено на паузу';
      await store.load();
    } catch { notice = 'Не удалось изменить расписание. Повторите при восстановлении связи'; }
    finally { busy = ''; }
  }

  async function remove() {
    if (busy) return;
    busy = 'delete'; notice = '';
    try {
      await deleteSchedule(schedule.id, crypto.randomUUID());
      removed();
    } catch { notice = 'Не удалось удалить расписание. Повторите при восстановлении связи'; }
    finally { busy = ''; }
  }

  const last = $derived(runs[0]);
  const target = $derived(targetLabel(schedule.target, sessions));
  // Текст отчёта свёрнут, пока владелец не раскроет: он может быть длинным.
  let openReports = $state<Record<string, boolean>>({});
</script>

<section data-schedule-detail class="detail" aria-label="Расписание: {schedule.name}">
  <a class="back" href="#/schedules" onclick={(event) => { event.preventDefault(); back(); }}>К расписанию</a>
  <div class="head">
    <h2>{schedule.name}</h2>
    <Button onclick={edit}>Изменить</Button>
  </div>
  <div class="facts">
    <span>Расписание</span><span>{cronHuman(schedule.cron)}</span>
    <span>Цель</span>
    <span data-role="schedule-target">{target.kind}{#if target.session && schedule.target.kind !== 'new_session'} · <a href={resolve('/c/[id]', { id: schedule.target.conversation_id })}>{target.session}</a>{/if}</span>
    <span>Пояс</span><span>{schedule.timezone}</span>
    <span>Следующий запуск</span><span>{fireTime(schedule.next_fire_at, schedule.timezone)} (пояс {schedule.timezone})</span>
    <span>Состояние</span><span data-tone={schedule.enabled ? 'ok' : 'warn'}>{schedule.enabled ? 'Включено' : 'На паузе'}</span>
    {#if last}{@const outcome = outcomeWord(last.state)}
      <span>Последний запуск</span><span data-tone={outcome.tone}>{outcome.word}</span>
    {/if}
  </div>
  {#if error}<p class="error" role="alert">{error}<Button onclick={() => void loadRuns()}>Повторить</Button></p>{/if}
  {#if notice}<p role="status">{notice}</p>{/if}

  <div class="actions">
    <Button variant="primary" {...(busy === 'run' ? { disabled: true as const, reason: 'Отправляю запрос…' } : { disabled: false as const })} onclick={() => void runNow(false)}>Запустить сейчас</Button>
    <Button {...(busy === 'run' ? { disabled: true as const, reason: 'Отправляю запрос…' } : { disabled: false as const })} onclick={() => void runNow(true)}>Запустить в любом случае</Button>
    <Button {...(busy === 'toggle' ? { disabled: true as const, reason: 'Изменяю…' } : { disabled: false as const })} onclick={() => void toggle()}>{schedule.enabled ? 'Поставить на паузу' : 'Возобновить'}</Button>
  </div>
  <p class="note">«Запустить в любом случае» пропускает предпроверку, проверки лимитов и пересечений; об этом останется запись в истории</p>

  <h3>История запусков <small>(время в поясе {schedule.timezone})</small></h3>
  {#if runs.length === 0 && !error}
    <p class="note">Запусков ещё не было</p>
  {/if}
  <div class="history" data-schedule-history>
    {#each runs as run (run.id)}
      {@const outcome = outcomeWord(run.state)}
      {@const detail = runDetails(run.detail)}
      <div class="run" data-run-row={run.id}>
        <div class="run-head">
          <span class="outcome" data-tone={outcome.tone}>{outcome.word}</span>
          <span class="when">{fireTime(run.scheduled_at, schedule.timezone)}</span>
          <span class="delay">Задержка: {run.delay_ms == null ? 'нет данных' : fmt.duration(run.delay_ms)}</span>
          {#if run.manual}<span class="tag">Вручную</span>{/if}
          {#if detail.runAnyway}<span class="tag" data-tone="warn">Без проверок</span>{/if}
        </div>
        <div class="run-body">
          {#if detail.code !== null}<span>Предпроверка: код {detail.code}</span>{/if}
          {#if detail.tail}<span class="tail">{detail.tail.slice(-512)}</span>{/if}
          {#if detail.window}<span>Окно лимита: {detail.window}</span>{/if}
          {#if detail.missedBefore > 0}<span>Пропущено до этого: {detail.missedBefore}</span>{/if}
          {#if detail.reason}<span>{detail.reason}</span>{/if}
          {#if run.session_id}
            <a href={resolve('/c/[id]', { id: run.session_id })}>Открыть сессию</a>
          {/if}
          {#if detail.reportText}
            <span class="toggle"><Button variant="text" onclick={() => (openReports[run.id] = !openReports[run.id])}>{openReports[run.id] ? 'Скрыть отчёт' : 'Показать отчёт'}</Button></span>
            {#if openReports[run.id]}
              <div class="run-report" data-role="run-report"><TextBlock block={{ id: `run-report-${run.id}`, text: detail.reportText, state: 'final' }} /></div>
            {/if}
          {/if}
        </div>
      </div>
    {/each}
  </div>

  <div class="delete">
    {#if confirming}
      <p role="alert">Удалить расписание вместе с историей запусков?</p>
      <div class="delete-actions">
        <Button onclick={() => confirming = false}>Отмена</Button>
        <Button {...(busy === 'delete' ? { disabled: true as const, reason: 'Удаляю…' } : { disabled: false as const })} onclick={() => void remove()}>Удалить</Button>
      </div>
    {:else}
      <Button onclick={() => confirming = true}>Удалить расписание</Button>
    {/if}
  </div>
  <p class="note">Сессии, которые расписание уже создало, останутся</p>
</section>

<style>
  .detail { display: grid; gap: var(--s4); }
  .back { min-height: var(--target); display: inline-flex; align-items: center; color: var(--accent-link); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: var(--s4); flex-wrap: wrap; }
  h2 { margin: var(--s0); font-size: var(--fs-title); font-weight: var(--weight-semibold); }
  h3 { margin: var(--s4) var(--s0) var(--s0); font-size: var(--fs-body); }
  .facts { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: var(--s3) var(--s5); padding: var(--s5); border-radius: var(--radius-card); background: var(--surface); }
  .facts > span:nth-child(odd) { color: var(--muted); font-size: var(--fs-sm); }
  [data-tone='ok'] { color: var(--success); }
  [data-tone='warn'] { color: var(--warning); }
  [data-tone='danger'] { color: var(--danger); }
  .error { margin: var(--s0); color: var(--danger); }
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
  .history { display: grid; gap: var(--s3); }
  .run { display: grid; gap: var(--s3); padding: var(--s5); border-radius: var(--radius-card); background: var(--surface); }
  .run-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--s2) var(--s5); }
  .outcome { font-weight: var(--weight-medium); }
  .when, .delay { color: var(--secondary); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .tag { color: var(--muted); font-size: var(--fs-sm); }
  .run-body { display: grid; gap: var(--s2); color: var(--secondary); font-size: var(--fs-sm); }
  .toggle { justify-self: start; }
  .run-report { min-width: 0; padding: var(--s4) var(--s5); border-radius: var(--radius-sm); background: var(--raised); color: var(--text); font-size: var(--fs-body); overflow-wrap: anywhere; }
  .facts a { color: var(--accent-link); }
  .tail { font-family: var(--font-mono); overflow-wrap: anywhere; }
  .run-body a { min-height: var(--target); display: inline-flex; align-items: center; color: var(--accent-link); }
  .delete { display: grid; gap: var(--s3); margin-top: var(--s6); }
  .delete-actions { display: flex; flex-wrap: wrap; gap: var(--s3); }
</style>
