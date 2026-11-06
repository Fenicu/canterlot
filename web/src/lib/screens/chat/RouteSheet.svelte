<script lang="ts">
  import { getRuntime } from '#lib/app/runtime.js';
  import type { ChatStore } from '#lib/stores/chat.svelte.js';
  import { asDecision, catalogTitles } from '#lib/routing/decision.js';
  import { routingTitles } from '#lib/routing/titles.svelte.js';
  import Sheet from '#lib/ui/Sheet.svelte';
  import RouteDecisionView from './RouteDecisionView.svelte';

  let { chat, onclose }: { chat: ChatStore; onclose: () => void } = $props();

  const decision = $derived(asDecision((chat.detail as { routing?: unknown } | null)?.routing));
  const titles = $derived.by(() => {
    try {
      return catalogTitles(getRuntime().machines.items.values());
    } catch {
      return catalogTitles([]);
    }
  });
</script>

<Sheet open title="Выбор маршрута" subtitle={decision ? `«${routingTitles.title(decision.task_type)}»` : undefined} {onclose}>
  {#if decision}
    <RouteDecisionView {decision} {titles} />
  {:else}
    <p>Сессия запущена без маршрута</p>
  {/if}
</Sheet>
