<script lang="ts">
  import { listJobs } from '#lib/api/client.js';
  import type { Runtime } from '#lib/app/runtime.js';
  import type { JobRecord } from '#lib/stores/machines.svelte.js';
  import WorkMotif from '#lib/ui/WorkMotif.svelte';
  import { durationWord, jobFacts, jobLabel, jobWord, machineJobs, offlineNote, sourceLine, type JobFacts } from '#lib/jobs/model.js';

  let { runtime, machineId, onopen }: { runtime: Runtime; machineId: string; onopen: (id: string) => void } = $props();

  // Несущая показывает и задачи, которые ещё никто не взял: машины у них нет, а хозяин раннера — она. Задачи её раннеров лежат на карточках раннеров.
  const carrier = $derived(runtime.machines.items.get(machineId)?.is_carrier === true);

  let failed = $state(false);
  // Список приходит один раз и после каждой перезагрузки снимка; дальше его ведут события хаба.
  $effect(() => {
    const machine = machineId;
    const waiting = carrier;
    void runtime.machines.jobsEpoch;
    failed = false;
    let alive = true;
    const take = ({ data }: { data: { items: JobRecord[] } }) => { if (alive) runtime.machines.mergeJobs(data.items); };
    void listJobs({ machine, limit: 50 }).then(take, () => { if (alive) failed = true; });
    if (waiting) void listJobs({ state: 'waiting_runner', limit: 50 }).then(take, () => { if (alive) failed = true; });
    return () => { alive = false; };
  });

  const rows = $derived(machineJobs([...runtime.machines.jobs.values()].map(jobFacts), machineId, { carrier }));
  const empty = $derived(rows.running.length === 0 && rows.recent.length === 0);
</script>

{#snippet row(job: JobFacts)}
  {@const word = jobWord(job)}
  {@const note = offlineNote(job)}
  {@const took = durationWord(job.durationMs)}
  {@const source = sourceLine(job.source)}
  {@const wants = job.state === 'waiting_runner' && job.labels.length > 0 ? `метки: ${job.labels.join(', ')}` : ''}
  <li>
    <button type="button" onclick={() => onopen(job.id)} data-job={job.id}>
      <span class="top">
        <!-- Задача идёт прямо сейчас: тот же мотив пера, что у очереди и строки хода. Машина без связи не подтверждает, что идёт. -->
        {#if word.live && !job.machineOffline}<span class="lead" data-job-mark><WorkMotif /></span>{/if}
        <span class="label">{jobLabel(job.title, job.shell) || job.id.slice(0, 8)}</span><span class="word" data-tone={word.tone}>{word.word}</span>
      </span>
      {#if source || wants || took || note}<span class="meta">{[source, wants, took, note].filter(Boolean).join(' · ')}</span>{/if}
    </button>
  </li>
{/snippet}

<section class="jobs" aria-labelledby="machine-jobs" data-machine-jobs>
  <h2 id="machine-jobs">Задачи</h2>
  {#if failed && empty}
    <p class="note">Не удалось загрузить задачи. Повторите при восстановлении связи</p>
  {:else if empty}
    <p class="note" data-role="jobs-empty">На этой машине нет задач</p>
  {:else}
    {#if rows.running.length > 0}
      <h3>Идут сейчас</h3>
      <ul>{#each rows.running as job (job.id)}{@render row(job)}{/each}</ul>
    {/if}
    {#if rows.recent.length > 0}
      <h3>Последние</h3>
      <ul>{#each rows.recent as job (job.id)}{@render row(job)}{/each}</ul>
    {/if}
  {/if}
</section>

<style>
  .jobs { min-width: 0; }
  h3 { margin: var(--s6) var(--s0) var(--s2); color: var(--muted); font-size: var(--fs-meta); font-weight: var(--weight-medium); }
  .note { color: var(--muted); font-size: var(--fs-sm); }
  ul { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  /* Перекрывает nowrap кнопок из secondary.css: строка источника (репозиторий @ коммит) должна переноситься, иначе раздвигает экран. Название держит свой nowrap с многоточием. */
  button { white-space: normal; display: flex; flex-direction: column; gap: var(--s1); width: 100%; min-width: 0; min-height: var(--target); padding: var(--s6) var(--s8); border-radius: var(--radius-card); background: var(--surface); color: var(--text); text-align: start; }
  .top { display: flex; align-items: baseline; gap: var(--s4); min-width: 0; }
  .lead { flex: none; align-self: center; display: inline-flex; }
  .label { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .word { flex: none; color: var(--secondary); font-size: var(--fs-sm); white-space: nowrap; }
  .word[data-tone='accent'] { color: var(--accent-link); }
  .word[data-tone='success'] { color: var(--success); }
  .word[data-tone='warning'] { color: var(--warning); }
  .word[data-tone='danger'] { color: var(--danger); }
  .word[data-tone='muted'] { color: var(--muted); }
  .meta { color: var(--muted); font-size: var(--fs-sm); overflow-wrap: anywhere; }
</style>
