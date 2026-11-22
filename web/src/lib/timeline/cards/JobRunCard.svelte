<script lang="ts">
  import { resolve } from '$app/paths';
  import { durationWord, isTerminal, jobFacts, jobHref, jobWord, offlineNote, outputsLine, reasonText, runnerLine, sourceLine } from '#lib/jobs/model.js';
  import { fetchJobOnce, runtimeOrNull } from '#lib/jobs/live.js';
  import WorkMotif from '#lib/ui/WorkMotif.svelte';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import CodeLines from './CodeLines.svelte';
  import CopyButton from './CopyButton.svelte';
  import type { JobCallData } from './defs/jobs';
  import type { CardProps } from './types';

  let { call, data, line, link, word, grows, env }: CardProps<JobCallData> = $props();
  /** Столько последних строк вывода видно в раскрытой карточке; остальное — в листе задачи. */
  const SHOWN = 30;

  const runtime = runtimeOrNull();
  // Строка задачи живая: событие хаба меняет её на месте; у задачи из истории её читают у хаба один раз.
  const live = $derived(data.jobId ? runtime?.machines.jobs.get(data.jobId) : undefined);
  const facts = $derived(live ? jobFacts(live) : null);
  $effect(() => {
    if (runtime && data.jobId && data.refusal === null && !isTerminal(data.state)) void fetchJobOnce(runtime, data.jobId);
  });

  const state = $derived(facts && call.state === 'done' && data.refusal === null ? jobWord(facts) : word);
  const exit = $derived(facts ? facts.exitCode : data.exitCode);
  const took = $derived(durationWord(facts ? facts.durationMs : data.durationMs));
  const finished = $derived(isTerminal(facts?.state ?? data.state));
  const note = $derived(facts ? offlineNote(facts) : data.offline && !finished ? 'машина не на связи' : '');
  const why = $derived(reasonText(facts?.endReason ?? data.reason));
  const stateNow = $derived(facts?.state ?? data.state);
  // Раннер называется, когда его выбирает хаб: до выбора строка говорит «ждёт раннер», после — имя. У команды на названной машине имя уже в шапке.
  const runner = $derived(data.refusal === null && (data.auto || stateNow === 'waiting_runner') ? runnerLine({ state: stateNow, machineName: facts ? facts.machineName : data.machine }) : '');
  const source = $derived(sourceLine(facts?.source ?? data.source));
  // Подвоз кода и сбор результатов идут не на раннере, а на несущей: тот же мотив пера, что у живой строки хода; без движения он стоит написанным.
  const staged = $derived(stateNow === 'staging' || stateNow === 'collecting');
  const meta = $derived(data.refusal === null ? [runner, source, facts?.workdir || data.workdir, finished && exit !== null ? `код ${exit}` : '', took, note, why].filter(Boolean) : []);
  const results = $derived(data.refusal === null ? outputsLine({ state: stateNow, outputsDir: facts ? facts.outputsDir : data.outputsDir || null, outputsNote: facts ? facts.outputsNote : data.outputsNote || null }) : null);
  const all = $derived(data.outputTail.replace(/\n$/, '').split('\n'));
  const lines = $derived(all.slice(-SHOWN).map((text) => ({ text })));
</script>

<!-- Команда на месте: строка называет машину и задачу, справа живое состояние, раскрытие показывает хвост вывода. -->
<CardShell {call} {env} {line} {grows} status={state?.word ?? ''} tone={state?.tone ?? 'muted'} live={state?.live === true} dot={state?.live === true} reveal="вывод">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if meta.length > 0}
    <p class="meta" data-role="job-meta">{#if staged}<span class="stage" data-role="job-stage"><WorkMotif /></span>{/if}<span class="text">{meta.join(' · ')}</span></p>
  {/if}
  {#if results}
    <div class="results" data-role="job-outputs">
      {#if results.dir}
        <div class="dir"><span class="cap">Результаты</span><code data-role="job-outputs-dir">{results.dir}</code><CopyButton text={results.dir} label="Скопировать путь к результатам" /></div>
      {/if}
      {#if results.note}<p class="note" data-role="job-outputs-note">{results.note}</p>{/if}
    </div>
  {/if}
  {#snippet fold()}
    {#if data.outputTail}
      {#if all.length > lines.length || data.truncated}<p class="note">Последние {lines.length} из {all.length} строк</p>{/if}
      <CodeLines {lines} lang={null} label="Вывод команды" />
    {:else if data.jobId && !finished}
      <p class="note" data-role="job-wait">Вывод придёт, когда команда закончится. Ход виден в листе задачи</p>
    {:else}
      <p class="note">Команда ничего не вывела</p>
    {/if}
    {#if data.jobId && data.refusal === null}<a class="open" href={resolve(jobHref(data.jobId) as '/')} data-role="job-open">Открыть задачу</a>{/if}
  {/snippet}
</CardShell>

<style>
  .meta { display: flex; align-items: center; gap: var(--s3); margin: var(--s0); min-width: 0; color: var(--secondary); }
  .meta .text { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .stage { flex: none; display: inline-flex; }
  .results { display: flex; flex-direction: column; gap: var(--s1); min-width: 0; }
  .dir { display: flex; align-items: center; gap: var(--s3); min-width: 0; }
  .cap { flex: none; color: var(--muted); font-size: var(--fs-sm); }
  code { min-width: 0; overflow-wrap: anywhere; font-family: var(--font-mono); font-size: var(--fs-sm); }
  .note { margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .open { align-self: flex-start; display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); text-decoration: none; }
  .open:hover, .open:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
