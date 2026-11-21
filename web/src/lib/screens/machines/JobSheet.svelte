<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { resolve } from '$app/paths';
  import { cancelJob, getJob, getJobLog } from '#lib/api/client.js';
  import { errorText } from '#lib/api/errors.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import { clock } from '#lib/clock.js';
  import { fmt } from '#lib/format/ru.js';
  import { durationWord, isTerminal, jobFacts, jobLabel, jobWord, offlineNote, outputsLine, reasonText, runnerLine, sourceLine } from '#lib/jobs/model.js';
  import { connectionStale } from '#lib/sync/stale.svelte.js';
  import { ticker } from '#lib/ticker.svelte.js';
  import Button from '#lib/ui/Button.svelte';
  import ConfirmDialog from '#lib/ui/ConfirmDialog.svelte';
  import Sheet from '#lib/ui/Sheet.svelte';
  import { swapOn } from '#lib/ui/swap.js';
  import WorkMotif from '#lib/ui/WorkMotif.svelte';
  import CodeLines from '#lib/timeline/cards/CodeLines.svelte';
  import CopyButton from '#lib/timeline/cards/CopyButton.svelte';

  let { runtime, jobId, onclose }: { runtime: Runtime; jobId: string; onclose: () => void } = $props();

  /** Живой хвост обновляется раз в столько миллисекунд, пока задача не закончилась. */
  const POLL_MS = 3000;
  const TAIL_BYTES = 16_384;
  const PAGE_BYTES = 65_536;
  /** Столько строк команды видно сразу; длинный скрипт раскрывается по кнопке. */
  const COMMAND_LINES = 8;

  const job = $derived(runtime.machines.jobs.get(jobId));
  const facts = $derived(job ? jobFacts(job) : null);
  const terminal = $derived(job ? isTerminal(job.state) : false);
  const now = $derived.by(() => { void ticker.second; return connectionStale.now() + clock.offset; });

  let missing = $state(false);
  let loadError = $state('');
  let text = $state('');
  /** Полный журнал убран по сроку хранения: остался хвост. */
  let expired = $state(false);
  let eof = $state(true);
  let nextOffset = $state(0);
  let paging = $state(false);
  let allCommand = $state(false);
  let confirming = $state(false);
  let cancelBusy = $state(false);
  let cancelError = $state('');
  let logEl = $state<HTMLElement | null>(null);
  // Ответ прежнего чтения не должен затереть более новый.
  let generation = 0;

  async function loadJob() {
    loadError = '';
    try {
      const { data } = await getJob(jobId);
      runtime.machines.mergeJobs([data.job]);
      if (!text) text = data.output_tail;
    } catch (err) {
      if (err && typeof err === 'object' && 'status' in err && (err as { status: number }).status === 404) missing = true;
      else loadError = errorText(err, 'Не удалось загрузить задачу. Повторите при восстановлении связи');
    }
  }

  /** Живой хвост: последние байты вывода, пока команда идёт. */
  async function readTail() {
    const mine = ++generation;
    try {
      const { data } = await getJobLog(jobId, { limit: TAIL_BYTES });
      if (mine !== generation) return;
      text = data.text;
      expired = data.note === 'full log expired';
      eof = true;
      await stickToEnd();
    } catch { /* следующий опрос повторит */ }
  }

  /** Журнал после конца: с начала, кусками; дальше по кнопке. */
  async function readLog(offset: number) {
    const mine = ++generation;
    paging = true;
    try {
      const { data } = await getJobLog(jobId, { offset, limit: PAGE_BYTES });
      if (mine !== generation) return;
      text = offset === 0 ? data.text : text + data.text;
      expired = data.note === 'full log expired';
      nextOffset = data.next_offset;
      eof = data.eof;
    } catch (err) {
      if (mine === generation) loadError = errorText(err, 'Не удалось прочитать журнал. Повторите при восстановлении связи');
    } finally { if (mine === generation) paging = false; }
  }

  async function stickToEnd() {
    await tick();
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  }

  onMount(() => {
    void loadJob();
    return ticker.retain();
  });

  // Идёт: хвост по опросу и сразу после каждого события; закончилась: весь журнал.
  $effect(() => {
    if (!job) return;
    void job.state; void job.output_bytes;
    if (terminal) { void readLog(0); return; }
    void readTail();
    const timer = setInterval(() => void readTail(), POLL_MS);
    return () => clearInterval(timer);
  });

  const title = $derived(job ? jobLabel(job.title, job.shell) || 'Задача' : 'Задача');
  const word = $derived(facts ? jobWord(facts) : null);
  const elapsed = $derived(job && !terminal && job.started_at ? fmt.duration(Math.max(0, now - Date.parse(job.started_at))) : '');
  const took = $derived(job ? durationWord(job.duration_ms) : '');
  const note = $derived(facts ? offlineNote(facts) : '');
  const why = $derived(job ? reasonText(job.end_reason) : '');
  const runner = $derived(facts ? runnerLine(facts) : '');
  const source = $derived(facts ? sourceLine(facts.source) : '');
  const results = $derived(facts ? outputsLine(facts) : null);
  const staged = $derived(job?.state === 'staging' || job?.state === 'collecting');
  const commandLines = $derived((job?.shell ?? '').split('\n'));
  const command = $derived((allCommand ? commandLines : commandLines.slice(0, COMMAND_LINES)).map((line) => ({ text: line })));
  const late = $derived(job?.late_report ? 'Воркер сообщил об итоге после потери связи; состояние осталось прежним' : '');
  /** Отменить можно идущую задачу и потерянную: команда уйдёт, когда машина вернётся на связь. */
  const cancellable = $derived(job ? (!terminal || (job.state === 'lost' && !job.cancel_requested)) : false);

  async function confirmCancel() {
    confirming = false; cancelBusy = true; cancelError = '';
    try {
      const { data } = await cancelJob(jobId, crypto.randomUUID());
      runtime.machines.mergeJobs([data]);
    } catch (err) {
      cancelError = errorText(err, 'Не удалось отменить задачу. Повторите при восстановлении связи');
    } finally { cancelBusy = false; }
  }
</script>

<Sheet open {title} subtitle={job ? [runner, job.workdir].filter(Boolean).join(' · ') : undefined} {onclose}>
  <div class="job" data-job-sheet>
    {#if missing}
      <p class="note" role="status">Такой задачи нет у хаба</p>
    {:else if !job}
      {#if loadError}<p class="error" role="alert">{loadError}</p><Button onclick={() => void loadJob()}>Повторить</Button>{:else}<p class="note" role="status">Загружаю задачу…</p>{/if}
    {:else}
      <dl class="facts">
        <div><dt>Состояние</dt><dd>{#if staged}<span class="stage" data-role="job-stage"><WorkMotif /></span>{/if}<span class="state" data-tone={word?.tone ?? 'muted'} data-role="job-state" use:swapOn={{ text: word?.word ?? '', key: job.state }}></span>{#if note}<span class="away" data-role="job-offline">{note}</span>{/if}</dd></div>
        <div><dt>{job.machine_id || !runner ? 'Машина' : 'Раннер'}</dt><dd data-role="job-machine">{runner}</dd></div>
        {#if source}<div><dt>Исходники</dt><dd class="mono" data-role="job-source">{source}</dd></div>{/if}
        {#if job.labels.length > 0}<div><dt>Метки</dt><dd data-role="job-labels">{job.labels.join(', ')}</dd></div>{/if}
        {#if job.workdir}<div><dt>Каталог</dt><dd class="mono" data-role="job-workdir">{job.workdir}</dd></div>{/if}
        {#if results?.dir}<div><dt>Результаты</dt><dd class="dir"><code data-role="job-outputs-dir">{results.dir}</code><CopyButton text={results.dir} label="Скопировать путь к результатам" /></dd></div>{/if}
        {#if results?.note}<div><dt>Результаты</dt><dd data-role="job-outputs-note">{results.note}</dd></div>{/if}
        {#if terminal && job.exit_code !== null && job.exit_code !== undefined}<div><dt>Код выхода</dt><dd data-role="job-exit">{job.exit_code}</dd></div>{/if}
        {#if took || elapsed}<div><dt>Длительность</dt><dd data-role="job-duration">{took || elapsed}</dd></div>{/if}
        {#if why}<div><dt>Причина</dt><dd data-role="job-reason">{why}</dd></div>{/if}
        {#if job.conversation_id}<div><dt>Заказчик</dt><dd><a href={resolve('/c/[id]', { id: job.conversation_id })}>{job.conversation_title || job.conversation_id.slice(0, 8)}</a></dd></div>{/if}
      </dl>
      {#if late}<p class="note" data-role="job-late">{late}</p>{/if}

      <section aria-label="Команда">
        <CodeLines lines={command} lang="bash" label="Команда" />
        {#if commandLines.length > COMMAND_LINES}
          <button type="button" class="more" aria-expanded={allCommand} onclick={() => allCommand = !allCommand}>{allCommand ? 'Свернуть команду' : `Вся команда · ${commandLines.length} ${fmt.plural(commandLines.length, ['строка', 'строки', 'строк'])}`}</button>
        {/if}
      </section>

      <section aria-label="Вывод">
        {#if expired}<p class="note" data-role="job-expired">Полный журнал удалён по сроку, остался хвост</p>{/if}
        {#if loadError}<p class="error" role="alert">{loadError}</p>{/if}
        <!-- Вывод не переносится: длинная строка листается вбок внутри блока, а не всей страницей. -->
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <pre class="log" bind:this={logEl} tabindex="0" role="region" aria-label="Вывод команды" aria-live={terminal ? 'off' : 'polite'} data-role="job-log">{text || (terminal ? 'Команда ничего не вывела' : 'Вывода пока нет')}</pre>
        {#if terminal && !eof}<Button onclick={() => void readLog(nextOffset)} busy={paging}>Показать дальше</Button>{/if}
      </section>

      {#if cancelError}<p class="error" role="alert">{cancelError}</p>{/if}
      {#if cancellable}
        <div class="actions">
          {#if cancelBusy}
            <Button disabled reason="Отменяю…" busy>Отменить</Button>
          {:else}
            <Button onclick={() => { confirming = true; }}>Отменить</Button>
          {/if}
        </div>
      {/if}
    {/if}
  </div>
</Sheet>

{#if confirming}
  <ConfirmDialog
    title="Отменить задачу?"
    body={job?.state === 'lost' ? 'Машина сейчас не на связи. Команда остановится, когда она вернётся' : 'Команда будет остановлена на машине вместе со своими процессами'}
    confirmLabel="Отменить задачу"
    danger
    onconfirm={() => void confirmCancel()}
    onclose={() => confirming = false}
  />
{/if}

<style>
  .job { display: flex; flex-direction: column; gap: var(--s6); min-width: 0; }
  .note { margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .error { margin: var(--s0); color: var(--danger); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .facts { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); font-size: var(--fs-sm); }
  .facts div { display: flex; flex-wrap: wrap; gap: var(--s1) var(--s4); min-width: 0; }
  dt { flex: none; min-width: var(--s15); color: var(--muted); }
  dd { flex: 1 1 0; min-width: 0; margin: var(--s0); overflow-wrap: anywhere; }
  .mono, code { font-family: var(--font-mono); }
  .stage { display: inline-flex; vertical-align: middle; margin-inline-end: var(--s3); }
  .dir { display: flex; align-items: center; gap: var(--s3); }
  .dir code { min-width: 0; overflow-wrap: anywhere; }
  .away { margin-inline-start: var(--s4); color: var(--warning); }
  .state[data-tone='accent'] { color: var(--accent-link); }
  .state[data-tone='success'] { color: var(--success); }
  .state[data-tone='warning'] { color: var(--warning); }
  .state[data-tone='danger'] { color: var(--danger); }
  .state[data-tone='muted'] { color: var(--muted); }
  section { display: flex; flex-direction: column; gap: var(--s3); min-width: 0; }
  .log { min-width: 0; max-width: 100%; max-height: calc(var(--fs-sm) * var(--lh-code) * 24); margin: var(--s0); padding: var(--s4) var(--s6); overflow: auto; border-radius: var(--radius-sm); background: var(--code); color: var(--text); font-family: var(--font-mono); font-size: var(--fs-sm); line-height: var(--lh-code); white-space: pre; overflow-wrap: normal; }
  .more { align-self: flex-start; min-height: var(--target); padding: var(--s0); background: var(--transparent); color: var(--accent-link); font-size: var(--fs-sm); }
  .actions { display: flex; flex-wrap: wrap; gap: var(--s4); }
  a { color: var(--accent-link); text-decoration: none; }
</style>
