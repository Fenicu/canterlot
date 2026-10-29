<script lang="ts">
  import StageTrack from '$lib/ui/StageTrack.svelte';
  import type { Stage } from '$lib/ui/stage-track';
  import type { InputView } from '$lib/stores/inputs';
  import { deliveryIndex, deliveryLabel } from '$lib/timeline/present';

  let { input, agentWorking = false }: { input: InputView; agentWorking?: boolean } = $props();
  const stages: Stage[] = [
    { id: 'saved', label: 'сохранено' },
    { id: 'machine', label: 'на машине' },
    { id: 'sending', label: 'передаётся' },
    { id: 'accepted', label: 'принято' },
    { id: 'working', label: 'агент работает' }
  ];
  const label = $derived(deliveryLabel(input.state, agentWorking));
  const index = $derived(deliveryIndex(input.state));
  const trackState = $derived(input.state === 'rejected' ? 'error' : input.state === 'accepted' && !agentWorking ? 'done' : 'active');
</script>

<article class="bubble" data-state={input.state}>
  <p>{input.text}</p>
  <StageTrack {stages} {index} state={trackState} form="compact" label="Доставка сообщения" status={label} />
  <p class="mark">{label}</p>
</article>

<style>
  .bubble {
    background: var(--accent-soft);
    border-radius: var(--radius-card);
    padding: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .mark { margin: 0; color: var(--text); }
  p { margin: 0; overflow-wrap: anywhere; }
</style>
