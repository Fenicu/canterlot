<script lang="ts">
  import { STATE_ORDER } from '#lib/screens/plan/planText.js';
  import CardLine from '../CardLine.svelte';
  import CardShell from '../CardShell.svelte';
  import { planTaskName, type PlanListData } from '../defs/plan';
  import type { CardProps } from '../types';
  import { livePlan, liveTask } from './livePlan';
  import PlanTaskRow from './PlanTaskRow.svelte';

  let { call, data, line, link, word, env }: CardProps<PlanListData> = $props();
  const plan = livePlan();
  const key = $derived(`plan-list:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  // Порядок экрана плана: что требует внимания, выше; внутри состояния — по номеру.
  const rank = (state: string | null) => (state ? STATE_ORDER.indexOf(state as never) : STATE_ORDER.length);
  const rows = $derived([...data.tasks].sort((a, b) => rank(a.state) - rank(b.state) || a.number - b.number));
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} live={word?.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.error}<p class="meta danger" data-role="plan-error">{data.error.text}</p>{/if}
  {#if rows.length > 0}
    {#if all}
      <ol class="rows" data-role="plan-list-tasks">
        {#each rows as task (task.id || task.number)}
          <PlanTaskRow orchestratorId={call.conversationId} number={task.number} name={planTaskName(task)} state={task.state} live={liveTask(plan, call.conversationId, task)?.state ?? null} />
        {/each}
      </ol>
    {/if}
    <button type="button" class="more" aria-expanded={all} data-role="plan-list-toggle" onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть' : `Весь план · ${rows.length}`}</button>
  {/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .danger { color: var(--danger); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
