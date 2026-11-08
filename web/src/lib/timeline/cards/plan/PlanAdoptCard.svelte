<script lang="ts">
  import CardLine from '../CardLine.svelte';
  import CardShell from '../CardShell.svelte';
  import { planTaskName, type PlanAdoptData } from '../defs/plan';
  import type { CardProps } from '../types';
  import { livePlan, liveTask } from './livePlan';
  import PlanTaskRow from './PlanTaskRow.svelte';

  let { call, data, line, link, word, env }: CardProps<PlanAdoptData> = $props();
  const SHOWN = 3;
  const plan = livePlan();
  const key = $derived(`plan-adopt:${call.block.id}`);
  const all = $derived(env.folds.isOpen(key));
  const shown = $derived(all || data.moved.length <= SHOWN + 1 ? data.moved : data.moved.slice(0, SHOWN));
</script>

<CardShell {call} {env} {line} status={word?.word ?? ''} tone={word?.tone ?? 'muted'} live={word?.live}>
  {#snippet head()}<CardLine {line} {link} />{/snippet}
  {#if data.error}<p class="meta danger" data-role="plan-error">{data.error.text}</p>{/if}
  {#if shown.length > 0}
    <ol class="rows" data-role="plan-adopt-moved">
      {#each shown as pair (pair.to)}
        {@const task = liveTask(plan, call.conversationId, { number: pair.to })}
        <PlanTaskRow orchestratorId={call.conversationId} number={pair.to} prefix={`#${pair.from} →`} name={planTaskName({ number: pair.to, title: task?.title ?? '' })} live={task?.state ?? null} />
      {/each}
    </ol>
    {#if data.moved.length > shown.length || all}
      <button type="button" class="more" aria-expanded={all} onclick={() => env.folds.toggle(key)}>{all ? 'Свернуть' : `Все номера · ${data.moved.length}`}</button>
    {/if}
  {/if}
</CardShell>

<style>
  p { margin: var(--s0); }
  .meta { color: var(--secondary); overflow-wrap: anywhere; }
  .danger { color: var(--danger); }
  .rows { display: flex; flex-direction: column; gap: var(--s2); margin: var(--s0); padding: var(--s0); list-style: none; }
  .more { align-self: flex-start; min-height: var(--target); margin-block: calc(var(--s2) * -1); padding: var(--s0); background: var(--transparent); color: var(--accent-link); }
</style>
