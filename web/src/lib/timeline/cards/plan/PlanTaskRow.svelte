<script lang="ts">
  import type { PlanTaskState } from '#lib/stores/plan.svelte.js';
  import PlanLink from './PlanLink.svelte';
  import PlanStateWord from './PlanStateWord.svelte';

  let {
    orchestratorId,
    number,
    name,
    prefix = '',
    state = null,
    live = null,
    quietState = null
  }: {
    orchestratorId: string;
    number: number;
    /** «#6 Название». */
    name: string;
    /** Что стоит перед ссылкой: старый номер при переносе плана. */
    prefix?: string;
    /** Состояние на момент вызова. */
    state?: PlanTaskState | null;
    /** Состояние сейчас по живому плану; отличается от `state` — показывается стрелкой. */
    live?: PlanTaskState | null;
    /** Состояние, которое не называется: у только что добавленных задач это «ждёт». */
    quietState?: PlanTaskState | null;
  } = $props();

  const moved = $derived(live !== null && state !== null && live !== state);
  const shown = $derived(moved ? state : (live ?? state));
</script>

<li class="row" data-plan-task={number}>
  {#if prefix}<span class="prefix">{prefix}</span>{/if}
  <PlanLink {orchestratorId} {number}>{name}</PlanLink>
  {#if shown && (moved || shown !== quietState)}
    <span class="states" data-role="plan-row-state">
      <PlanStateWord state={shown} />{#if moved && live}<span class="arrow" aria-hidden="true">→</span><span class="sr-only">, сейчас </span><PlanStateWord state={live} />{/if}
    </span>
  {/if}
</li>

<style>
  .row { display: flex; align-items: baseline; gap: var(--s3); min-width: 0; }
  .prefix { flex: none; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .states { display: inline-flex; flex: none; align-items: baseline; gap: var(--s2); margin-inline-start: auto; padding-inline-start: var(--s3); }
  .arrow { color: var(--muted); }
</style>
