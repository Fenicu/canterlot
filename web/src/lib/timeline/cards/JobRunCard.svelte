<script lang="ts">
  import { resolve } from '$app/paths';
  import { durationWord, isTerminal, jobFacts, jobHref, jobWord, offlineNote, reasonText } from '#lib/jobs/model.js';
  import { fetchJobOnce, runtimeOrNull } from '#lib/jobs/live.js';
  import CardLine from './CardLine.svelte';
  import CardShell from './CardShell.svelte';
  import CodeLines from './CodeLines.svelte';
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
  const meta = $derived(data.refusal === null ? [data.workdir, finished && exit !== null ? `код ${exit}` : '', took, note, why].filter(Boolean) : []);
  const all = $derived(data.outputTail.replace(/\n$/, '').split('\n'));
  const lines = $derived(all.slice(-SHOWN).map((text) => ({ text })));
</script>

<!-- Команда на месте: строка называет машину и задачу, справа живое состояние, раскрытие показывает хвост вывода. -->
<CardShell {call} {env} {line} {grows} status={state?.word ?? ''} tone={state?.tone ?? 'muted'} live={state?.live === true} dot={state?.live === true} reveal="вывод">
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if meta.length > 0}<p class="meta" data-role="job-meta">{meta.join(' · ')}</p>{/if}
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
  .meta { margin: var(--s0); min-width: 0; overflow: hidden; color: var(--secondary); text-overflow: ellipsis; white-space: nowrap; }
  .note { margin: var(--s0); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
  .open { align-self: flex-start; display: inline-flex; align-items: center; min-height: var(--target); color: var(--accent-link); text-decoration: none; }
  .open:hover, .open:focus-visible { text-decoration: underline; text-underline-offset: var(--s1); }
</style>
