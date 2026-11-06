<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import type { BlockModel } from '#lib/stores/blocks.js';
  import { asDecision, catalogTitles, routeLine } from '#lib/routing/decision.js';
  import { itemReason } from '#lib/routing/queue.js';
  import { parseSpawnAnswer } from '#lib/routing/spawnAnswer.js';
  import { routingTitles } from '#lib/routing/titles.svelte.js';

  let { result }: { result: BlockModel | null } = $props();

  let runtime: ReturnType<typeof getRuntime> | null = null;
  try {
    runtime = getRuntime();
  } catch {
    runtime = null;
  }

  // Ответ хаба приходит с результатом вызова и после него не меняется.
  const answer = $derived(result && result.state !== 'streaming' ? parseSpawnAnswer(result.text) : null);
  const routed = $derived(answer?.status === 'started' && answer.routing ? answer : null);
  $effect(() => {
    if (routed) routingTitles.ensure(runtime?.list.routingRevision ?? null);
  });
  const line = $derived.by(() => {
    if (!answer) return null;
    if (answer.status === 'queued')
      return `Запуск в очереди: ${itemReason({ reason: answer.reason ?? '', detail: answer.detail ?? '' })}`;
    if (!routed) return null;
    // Полное решение с пропусками хранит строка ребёнка; в ответе инструмента только выбор.
    const row = routed.session_id ? runtime?.list.items.get(routed.session_id) : undefined;
    const decision = asDecision(row?.routing) ?? asDecision({ considered: [], ...routed.routing });
    if (!decision) return null;
    const titles = catalogTitles(runtime?.machines.items.values() ?? []);
    return routeLine(decision, routingTitles.title(decision.task_type), titles);
  });
</script>

{#if line}
  <p class="route" data-role={answer?.status === 'queued' ? 'spawn-queued-line' : 'spawn-route-line'}>{line}</p>
{/if}
{#if answer?.status === 'started' && answer.machine_metrics === 'stale'}
  <p class="route" data-role="spawn-metrics-stale">Метрики машины устарели: страж не проверял запас</p>
{/if}

<style>
  .route { margin: var(--s0); padding: var(--s1) var(--s6) var(--s2); color: var(--secondary); font-size: var(--fs-sm); overflow-wrap: anywhere; }
</style>
