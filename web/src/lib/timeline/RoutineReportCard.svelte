<script lang="ts">
  import { onDestroy } from 'svelte';
  import { dismissRoutineReport, runSchedule } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import { fmt } from '#lib/format/ru.js';
  import { intents } from '#lib/ops/intent.js';
  import { COPY_DONE, COPY_FAILED } from '#lib/render/copy.js';
  import { failureText, type RoutineReport } from '#lib/routines/reports.js';
  import TextBlock from '#lib/timeline/TextBlock.svelte';
  import Button from '#lib/ui/Button.svelte';
  import WorkMotif from '#lib/ui/WorkMotif.svelte';

  // Карточка только владельца: не сообщение, без закладки и «Об ответе», в очередь и состояние сессии не входит.
  let { report, highlight = false, onDismissed }: {
    report: RoutineReport;
    highlight?: boolean;
    /** Закрытие подтверждено ответом хаба: карточка уходит из ленты, не дожидаясь события. */
    onDismissed?: (id: string) => void;
  } = $props();

  let busy = $state<'dismiss' | 'retry' | null>(null);
  let failed = $state('');
  let notice = $state('');
  let copied = $state('');
  let timer: ReturnType<typeof setTimeout> | null = null;

  const when = $derived(fmt.clock(Date.parse(report.created_at) || 0));

  async function dismiss() {
    if (busy) return;
    busy = 'dismiss';
    failed = '';
    try {
      await intents.run(`routine-dismiss:${report.id}`, (key) => dismissRoutineReport(report.conversation_id, report.id, key));
      onDismissed?.(report.id);
    } catch (error) {
      failed = errorText(error, 'Не удалось завершить. Повторите при восстановлении связи');
    } finally {
      busy = null;
    }
  }

  async function retry() {
    if (busy) return;
    busy = 'retry';
    failed = '';
    notice = '';
    try {
      await intents.run(`routine-retry:${report.id}`, (key) => runSchedule(report.schedule_id, { run_anyway: false }, key));
      notice = 'Запуск принят';
    } catch (error) {
      failed = errorText(error, 'Не удалось запустить рутину. Повторите той же кнопкой');
    } finally {
      busy = null;
    }
  }

  async function copy() {
    try {
      if (typeof navigator === 'undefined' || !navigator.clipboard?.writeText) throw new Error('clipboard_unavailable');
      await navigator.clipboard.writeText(report.body ?? '');
      copied = COPY_DONE;
    } catch {
      copied = COPY_FAILED;
    }
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      copied = '';
      timer = null;
    }, 2000);
  }

  onDestroy(() => {
    if (timer) clearTimeout(timer);
  });
</script>

<article class="report" data-role="routine-report" data-state={report.state} data-report-id={report.id} data-highlight={highlight ? '' : undefined} aria-label={`Рутина · ${report.title}`}>
  <header class="head">
    <h3 class="title">Рутина · {report.title}</h3>
    <time class="when" datetime={report.created_at}>{when}</time>
  </header>

  {#if report.state === 'pending'}
    <p class="pending" data-role="routine-pending"><WorkMotif /><span>Рутина собирает отчёт…</span></p>
  {:else if report.state === 'ready'}
    <div class="body" data-role="routine-body">
      <TextBlock block={{ id: `routine-${report.id}`, text: report.body ?? '', state: 'final' }} />
    </div>
    <div class="actions">
      <Button variant="primary" busy={busy === 'dismiss'} onclick={() => void dismiss()}>Завершить</Button>
      <Button onclick={() => void copy()}>Скопировать</Button>
      <span class="copied" role="status" data-failed={copied === COPY_FAILED ? '' : undefined}>{copied}</span>
    </div>
  {:else}
    <p class="error" data-role="routine-error">{failureText(report.error)}</p>
    <div class="actions">
      <Button variant="primary" busy={busy === 'retry'} onclick={() => void retry()}>Повторить</Button>
      <Button busy={busy === 'dismiss'} onclick={() => void dismiss()}>Завершить</Button>
    </div>
  {/if}
  {#if notice}<p class="note" role="status">{notice}</p>{/if}
  {#if failed}<p class="problem" role="alert">{failed}</p>{/if}
</article>

<style>
  .report {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    min-width: 0;
    padding: var(--s5) var(--s6);
    border-radius: var(--radius-card);
    background: var(--surface);
  }
  .head { display: flex; align-items: baseline; justify-content: space-between; gap: var(--s4); min-width: 0; }
  .title { margin: var(--s0); min-width: 0; color: var(--text); font-size: var(--fs-body); font-weight: var(--weight-semibold); line-height: var(--lh-tight); overflow-wrap: anywhere; }
  .when { flex: none; color: var(--muted); font-size: var(--fs-sm); font-variant-numeric: tabular-nums; }
  .pending { display: flex; align-items: center; gap: var(--s3); margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); }
  .body { min-width: 0; overflow-wrap: anywhere; }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--s3); }
  .copied { color: var(--secondary); font-size: var(--fs-sm); }
  .copied:empty { display: none; }
  .copied[data-failed] { color: var(--danger); }
  .error { margin: var(--s0); color: var(--warning); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .note { margin: var(--s0); color: var(--muted); font-size: var(--fs-sm); }
  .problem { margin: var(--s0); color: var(--danger); font-size: var(--fs-sm); overflow-wrap: anywhere; }
</style>
