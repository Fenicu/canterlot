<script lang="ts">
  import CardLine from '../CardLine.svelte';
  import CardShell from '../CardShell.svelte';
  import { planTaskName, type PlanAddData } from '../defs/plan';
  import type { CardProps } from '../types';
  import { livePlan, liveTask } from './livePlan';
  import PlanTaskRow from './PlanTaskRow.svelte';

  let { call, data, line, link, word, env }: CardProps<PlanAddData> = $props();
  const SHOWN = 3;
  const plan = livePlan();
  const key = $derived(`plan-add:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  const tasks = $derived(data.tasks.filter((task) => task.number > 0 || task.title));
  // Список короче четырёх строк стоит целиком; длинный раскрывается внутри карточки.
  const shown = $derived(all || tasks.length <= SHOWN + 1 ? tasks : tasks.slice(0, SHOWN));
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} live={word?.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.error}<p class="meta danger" data-role="plan-error">{data.error.text}</p>{/if}
  {#if !data.error && shown.length > 0}
    <ol class="rows" data-role="plan-add-tasks">
      {#each shown as task, index (index + ':' + task.number)}
        {#if task.number > 0}
          <PlanTaskRow orchestratorId={call.conversationId} number={task.number} name={planTaskName(task)} state={task.state} live={liveTask(plan, call.conversationId, task)?.state ?? null} quietState="waiting" />
        {:else}
          <li class="pending">{task.title}</li>
        {/if}
      {/each}
    </ol>
    {#if tasks.length > shown.length || all}
      <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть' : `Все задачи · ${tasks.length}`}</button>
    {/if}
  {/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .danger { color: var(--danger); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .pending { overflow: hidden; color: var(--secondary); text-overflow: ellipsis; white-space: nowrap; }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
