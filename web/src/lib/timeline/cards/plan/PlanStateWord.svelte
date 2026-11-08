<script lang="ts">
  import { STATE_TONE, STATE_WORD } from '#lib/screens/plan/planText.js';
  import type { PlanTaskState } from '#lib/stores/plan.svelte.js';

  let { state, arriving = false }: { state: PlanTaskState; arriving?: boolean } = $props();
</script>

<!-- Слово и цвет состояния — как у строки задачи на экране плана. -->
<span class="state" data-tone={STATE_TONE[state]} data-state={state} data-arriving={arriving ? '' : undefined}>{STATE_WORD[state]}</span>

<style>
  .state { flex: none; color: var(--secondary); white-space: nowrap; transition: color var(--duration-fast) var(--ease); }
  .state[data-tone='accent'] { color: var(--accent-link); }
  .state[data-tone='blue'] { color: var(--blue); }
  .state[data-tone='warning'] { color: var(--warning); }
  .state[data-tone='success'] { color: var(--success); }
  .state[data-tone='danger'] { color: var(--danger); }
  .state[data-tone='muted'] { color: var(--muted); }
  /* Новое состояние, пришедшее на глазах, входит коротким сдвигом; история стоит сразу. */
  .state[data-arriving] { display: inline-block; animation: plan-state-in var(--duration-label-in) var(--ease-label) backwards; }
  @keyframes plan-state-in { from { opacity: 0; transform: translateX(calc(var(--shift-label) * -1)); } }
  @media (prefers-reduced-motion: reduce) { .state[data-arriving] { animation: none; } }
</style>
